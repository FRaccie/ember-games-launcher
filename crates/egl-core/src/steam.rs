//! Finding Steam, its libraries, the games in them and its user folders.

use crate::catalog::SteamApp;
use crate::vdf;
use std::path::{Path, PathBuf};

/// Where a game was found.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GameInstall {
    pub appid: u32,
    /// The Steam library that holds it (its steamapps folder's parent).
    pub library: PathBuf,
    /// The game's own folder.
    pub dir: PathBuf,
    pub exe: PathBuf,
    /// Found from a folder the player picked, not from Steam.
    pub manual: bool,
}

/// Steam folders that exist on this PC, best first.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, enums::HKEY_LOCAL_MACHINE, RegKey};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                out.push(PathBuf::from(p.replace('/', "\\")));
            }
        }
        if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\WOW6432Node\Valve\Steam") {
            if let Ok(p) = k.get_value::<String, _>("InstallPath") {
                out.push(PathBuf::from(p));
            }
        }
        out.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    }
    #[cfg(not(windows))]
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".local/share/Steam"));
        out.push(home.join(".steam/steam"));
        out.push(home.join(".steam/debian-installation"));
        out.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
    }
    dedupe(out.into_iter().filter(|p| p.join("steamapps").is_dir()).collect())
}

/// Whether two paths are the same folder (case and slashes aside when they do not exist).
pub fn same_folder(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['/', '\\']).to_lowercase().replace('\\', "/");
    norm(a) == norm(b)
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !out.iter().any(|q| same_folder(q, &p)) {
            out.push(p);
        }
    }
    out
}

/// Library folders named in the text of libraryfolders.vdf, in file order.
/// Reads the nested format and the older flat one. Bad text yields nothing.
pub fn parse_library_folders(text: &str) -> Vec<PathBuf> {
    let Ok(root) = vdf::parse_text(text) else { return Vec::new() };
    let Some(lf) = root.map("libraryfolders") else { return Vec::new() };
    lf.0.iter()
        .filter_map(|(_, v)| match v {
            vdf::Value::Map(m) => m.str("path").map(PathBuf::from),
            vdf::Value::Str(s) if s.contains(['/', '\\']) => Some(PathBuf::from(s)),
            _ => None,
        })
        .collect()
}

/// The Steam folder itself, then every library it lists that exists.
pub fn libraries(steam: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam.to_path_buf()];
    if let Ok(text) = std::fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")) {
        out.extend(parse_library_folders(&text).into_iter().filter(|p| p.join("steamapps").is_dir()));
    }
    dedupe(out)
}

/// The installdir of an app manifest, when it names that app (or none) and
/// is a plain folder name.
pub fn manifest_installdir(text: &str, appid: u32) -> Option<String> {
    let root = vdf::parse_text(text).ok()?;
    let state = root.map("AppState")?;
    if let Some(id) = state.str("appid") {
        if id.trim() != appid.to_string() {
            return None;
        }
    }
    let dir = state.str("installdir")?.trim();
    let plain = !dir.is_empty() && dir != "." && dir != ".." && !dir.contains(['/', '\\', ':']);
    plain.then(|| dir.to_string())
}

/// The game's exe inside dir, when it is there.
pub fn exe_in(dir: &Path, app: &SteamApp) -> Option<PathBuf> {
    let exe = dir.join(&app.exe);
    exe.is_file().then_some(exe)
}

/// Looks for one of the apps in every library, in the order the apps are
/// listed (the catalog lists the version players use most first).
pub fn find_game(roots: &[PathBuf], apps: &[SteamApp]) -> Option<GameInstall> {
    let libs: Vec<PathBuf> = dedupe(roots.iter().flat_map(|r| libraries(r)).collect());
    for app in apps {
        for lib in &libs {
            let common = lib.join("steamapps").join("common");
            let mut folders = Vec::new();
            if let Ok(text) =
                std::fs::read_to_string(lib.join("steamapps").join(format!("appmanifest_{}.acf", app.appid)))
            {
                folders.extend(manifest_installdir(&text, app.appid));
            }
            folders.extend(app.folders.iter().cloned());
            for f in folders {
                let dir = common.join(&f);
                if let Some(exe) = exe_in(&dir, app) {
                    return Some(GameInstall { appid: app.appid, library: lib.clone(), dir, exe, manual: false });
                }
            }
        }
    }
    None
}

/// A folder the player picked: the game folder, or the folder that holds the
/// exe (so SFV's Binaries\Win64 also works).
pub fn from_folder(picked: &Path, apps: &[SteamApp]) -> Option<GameInstall> {
    for app in apps {
        let mut dir = picked.to_path_buf();
        for _ in 0..4 {
            if let Some(exe) = exe_in(&dir, app) {
                let library = dir
                    .ancestors()
                    .find(|a| a.file_name().is_some_and(|n| n.eq_ignore_ascii_case("steamapps")))
                    .and_then(|s| s.parent())
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                return Some(GameInstall { appid: app.appid, library, dir, exe, manual: true });
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

/// userdata/<id> folders of accounts that logged in here (0 is a placeholder).
pub fn users(steam: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(steam.join("userdata"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.is_empty() && name != "0" && name.bytes().all(|b| b.is_ascii_digit()) && p.join("config").is_dir()
        })
        .collect();
    out.sort();
    out
}

/// The game's Proton prefix (Linux): <library>/steamapps/compatdata/<appid>/pfx.
pub fn proton_prefix(game: &GameInstall) -> PathBuf {
    game.library.join(format!("steamapps/compatdata/{}/pfx", game.appid))
}

/// Whether Steam is running. Steam rewrites shortcuts.vdf and localconfig.vdf
/// from memory when it exits, so they are only edited while it is closed.
pub fn steam_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("steam.exe"))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("pgrep").args(["-x", "steam"]).output().map(|o| o.status.success()).unwrap_or(false)
    }
}

/// Which of these exe file names (case ignored) are running now: one look at
/// the process list. On Linux a Proton game shows its Windows exe name in the
/// command line, so that is matched too.
pub fn running(names: &[String]) -> Vec<String> {
    let names: Vec<String> = names.iter().filter(|n| !n.is_empty()).map(|n| n.to_lowercase()).collect();
    if names.is_empty() {
        return Vec::new();
    }
    #[cfg(windows)]
    let list = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
            .unwrap_or_default()
    };
    #[cfg(not(windows))]
    let list = std::process::Command::new("ps")
        .args(["-eo", "args="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    running_in(&list, &names)
}

/// The names found in a process listing as whole file names: after a quote,
/// a path separator or the line start, and before a quote, a space or the
/// line end.
fn running_in(list: &str, names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for n in names {
        let hit = list.lines().any(|l| {
            l.match_indices(n.as_str()).any(|(i, _)| {
                let before = l[..i].chars().last();
                let after = l[i + n.len()..].chars().next();
                matches!(before, None | Some('"' | '\\' | '/' | ' ')) && matches!(after, None | Some('"' | ' '))
            })
        });
        if hit && !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

/// Asks Steam to close and waits up to `wait` for it to go.
pub fn shutdown_steam(steam: &Path, wait: std::time::Duration) -> bool {
    let exe = if cfg!(windows) { steam.join("steam.exe") } else { PathBuf::from("steam") };
    let _ = std::process::Command::new(exe).arg("-shutdown").spawn();
    let end = std::time::Instant::now() + wait;
    while std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !steam_running() {
            return true;
        }
    }
    false
}

pub fn start_steam(steam: &Path) {
    let exe = if cfg!(windows) { steam.join("steam.exe") } else { PathBuf::from("steam") };
    let _ = std::process::Command::new(exe).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(appid: u32, exe: &str, folder: &str) -> SteamApp {
        SteamApp { appid, exe: exe.into(), folders: vec![folder.into()], label: None }
    }

    #[test]
    fn library_folders_both_formats() {
        let nested =
            "\"libraryfolders\" { \"0\" { \"path\" \"/a\" } \"1\" { \"path\" \"/b\" } \"contentstatsid\" \"9\" }";
        assert_eq!(parse_library_folders(nested), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        let flat = "\"LibraryFolders\" { \"TimeNextStatsReport\" \"1\" \"1\" \"D:\\\\Lib\" }";
        assert_eq!(parse_library_folders(flat), vec![PathBuf::from(r"D:\Lib")]);
        assert!(parse_library_folders("garbage {").is_empty());
    }

    #[test]
    fn manifest_installdir_checks_app_and_name() {
        let m = |id: &str, dir: &str| format!("\"AppState\" {{ \"appid\" \"{id}\" \"installdir\" \"{dir}\" }}");
        assert_eq!(manifest_installdir(&m("45760", "SSF4"), 45760).as_deref(), Some("SSF4"));
        assert_eq!(manifest_installdir(&m("1", "SSF4"), 45760), None);
        assert_eq!(manifest_installdir(&m("45760", "..\\x"), 45760), None);
    }

    #[test]
    fn finds_game_by_manifest_then_default_folder_and_prefers_first_app() {
        let t = tempfile::tempdir().unwrap();
        let steam = t.path().join("Steam");
        let lib = t.path().join("Lib");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}",
                steam.display().to_string().replace('\\', "\\\\"),
                lib.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        // The Steam re-release in the Steam folder, by its default folder name.
        std::fs::create_dir_all(steam.join("steamapps/common/SFxT")).unwrap();
        std::fs::write(steam.join("steamapps/common/SFxT/SFTK.exe"), b"").unwrap();
        // The GFWL version in the second library, under a manifest-named folder.
        std::fs::create_dir_all(lib.join("steamapps/common/Renamed")).unwrap();
        std::fs::write(lib.join("steamapps/common/Renamed/SFTK.exe"), b"").unwrap();
        std::fs::write(
            lib.join("steamapps/appmanifest_55550.acf"),
            "\"AppState\" { \"appid\" \"55550\" \"installdir\" \"Renamed\" }",
        )
        .unwrap();

        let apps = [app(55550, "SFTK.exe", "Street Fighter X Tekken"), app(209120, "SFTK.exe", "SFxT")];
        let g = find_game(std::slice::from_ref(&steam), &apps).unwrap();
        assert_eq!(g.appid, 55550);
        assert_eq!(g.dir, lib.join("steamapps/common/Renamed"));
        assert!(same_folder(&g.library, &lib));

        let only_steam = [app(209120, "SFTK.exe", "SFxT")];
        assert_eq!(find_game(&[steam], &only_steam).unwrap().appid, 209120);
    }

    #[test]
    fn picked_folder_walks_up_to_the_game_folder() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("steamapps/common/StreetFighterV");
        std::fs::create_dir_all(game.join("StreetFighterV/Binaries/Win64")).unwrap();
        std::fs::write(game.join("StreetFighterV/Binaries/Win64/StreetFighterV.exe"), b"").unwrap();
        let apps = [app(310950, "StreetFighterV/Binaries/Win64/StreetFighterV.exe", "StreetFighterV")];
        let g = from_folder(&game.join("StreetFighterV/Binaries/Win64"), &apps).unwrap();
        assert_eq!(g.dir, game);
        assert_eq!(g.library, t.path());
        assert!(g.manual);
        assert!(from_folder(t.path(), &apps).is_none());
    }
    #[test]
    fn running_matches_whole_exe_names() {
        let names = vec!["ssfiv.exe".to_string(), "sftk.exe".to_string()];
        let win = "\"steam.exe\",\"123\"\n\"SSFIV.exe\",\"456\"".to_lowercase();
        assert_eq!(running_in(&win, &names), vec!["ssfiv.exe"]);
        let linux = "/home/d/.steam/proton/files/bin/wine64 z:\\games\\sfxt\\sftk.exe -ember\nbash".to_lowercase();
        assert_eq!(running_in(&linux, &names), vec!["sftk.exe"]);
        assert!(running_in("\"notssfiv.exe\",\"1\"\nssfiv.exe.bak", &names).is_empty());
    }
}
