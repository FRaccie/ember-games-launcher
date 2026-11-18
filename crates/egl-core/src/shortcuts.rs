//! Adding a mod to Steam as its own entry (a non-Steam shortcut) with its
//! pictures, for every Steam user on this PC. Steam must be closed.

use crate::catalog::Game;
use crate::launch::LaunchError;
use crate::steam::{self, GameInstall};
use crate::vdf::{self, Bin};
use std::path::{Path, PathBuf};

/// What the Steam entry starts.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub exe: String,
    pub start_dir: String,
    pub launch_options: String,
    pub icon: String,
}

/// Steam's id for a shortcut: the CRC of its quoted exe and its name, with
/// the top bit set.
pub fn shortcut_appid(exe_quoted: &str, name: &str) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(exe_quoted.as_bytes());
    h.update(name.as_bytes());
    h.finalize() | 0x8000_0000
}

/// steam://rungameid/<id> for a shortcut.
pub fn rungameid(appid: u32) -> u64 {
    ((appid as u64) << 32) | 0x0200_0000
}

fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// The entry for a mod. Windows: its launcher. Linux: a small script that
/// asks Steam to start the game with -ember (see launch.rs).
pub fn entry_for(data: &Path, game: &Game, install: &GameInstall, dir: &Path) -> Result<Entry, LaunchError> {
    let name = game.module.name.clone();
    let icon = game.module.art.icon.as_deref().map(|i| dir.join(i)).filter(|p| p.is_file());
    if cfg!(windows) {
        let args = crate::launch::windows_args(game, install)
            .iter()
            .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Entry {
            name,
            exe: quoted(&dir.join(&game.module.launcher)),
            start_dir: quoted(dir),
            launch_options: args,
            icon: icon.map(|p| p.display().to_string()).unwrap_or_default(),
        })
    } else {
        let script = data.join(format!("play-{}.sh", game.id));
        let body = format!("#!/bin/sh\n# {name}: starts the game through Steam with -ember (Ember Games Launcher).\nexec steam -applaunch {} -ember\n", install.appid);
        crate::install::write_atomic(&script, body.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(Entry {
            name,
            exe: quoted(&script),
            start_dir: quoted(data),
            launch_options: String::new(),
            icon: icon.map(|p| p.display().to_string()).unwrap_or_default(),
        })
    }
}

fn shortcut_map(e: &Entry, appid: u32) -> Bin {
    let s = |v: &str| Bin::Str(v.to_string());
    Bin::Map(vec![
        ("appid".into(), Bin::Int(appid as i32)),
        ("AppName".into(), s(&e.name)),
        ("Exe".into(), s(&e.exe)),
        ("StartDir".into(), s(&e.start_dir)),
        ("icon".into(), s(&e.icon)),
        ("ShortcutPath".into(), s("")),
        ("LaunchOptions".into(), s(&e.launch_options)),
        ("IsHidden".into(), Bin::Int(0)),
        ("AllowDesktopConfig".into(), Bin::Int(1)),
        ("AllowOverlay".into(), Bin::Int(1)),
        ("OpenVR".into(), Bin::Int(0)),
        ("Devkit".into(), Bin::Int(0)),
        ("DevkitGameID".into(), s("")),
        ("DevkitOverrideAppID".into(), Bin::Int(0)),
        ("LastPlayTime".into(), Bin::Int(0)),
        ("FlatpakAppID".into(), s("")),
        ("tags".into(), Bin::Map(vec![("0".into(), s("Ember"))])),
    ])
}

/// shortcuts.vdf with the entry added, or the one with the same name
/// replaced. Other shortcuts keep their slots.
pub fn upsert(file: &Bin, e: &Entry) -> (Bin, u32) {
    let appid = shortcut_appid(&e.exe, &e.name);
    let mut root = match file {
        Bin::Map(p) => p.clone(),
        _ => Vec::new(),
    };
    if !root.iter().any(|(k, _)| k.eq_ignore_ascii_case("shortcuts")) {
        root.push(("shortcuts".into(), Bin::Map(Vec::new())));
    }
    let list = root.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("shortcuts")).map(|(_, v)| v).unwrap();
    let Bin::Map(items) = list else { unreachable!() };
    let same = |v: &Bin| {
        v.get("appname").and_then(Bin::as_str) == Some(e.name.as_str())
            || v.get("appid").and_then(Bin::as_int) == Some(appid as i32)
    };
    match items.iter_mut().find(|(_, v)| same(v)) {
        Some((_, v)) => *v = shortcut_map(e, appid),
        None => {
            let next = items.iter().filter_map(|(k, _)| k.parse::<u32>().ok()).max().map_or(0, |m| m + 1);
            items.push((next.to_string(), shortcut_map(e, appid)));
        }
    }
    (Bin::Map(root), appid)
}

/// shortcuts.vdf without the entry named `name`, slots renumbered.
pub fn remove(file: &Bin, name: &str) -> Bin {
    let Bin::Map(root) = file else { return file.clone() };
    let root = root
        .iter()
        .map(|(k, v)| match v {
            Bin::Map(items) if k.eq_ignore_ascii_case("shortcuts") => {
                let kept: Vec<_> = items
                    .iter()
                    .filter(|(_, v)| v.get("appname").and_then(Bin::as_str) != Some(name))
                    .enumerate()
                    .map(|(i, (_, v))| (i.to_string(), v.clone()))
                    .collect();
                (k.clone(), Bin::Map(kept))
            }
            _ => (k.clone(), v.clone()),
        })
        .collect();
    Bin::Map(root)
}

fn shortcuts_file(user: &Path) -> PathBuf {
    user.join("config/shortcuts.vdf")
}

fn read_file(path: &Path) -> Result<Bin, LaunchError> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    vdf::parse_binary(&data).map_err(|e| LaunchError::Vdf(path.display().to_string(), e))
}

fn write_file(path: &Path, value: &Bin) -> Result<(), LaunchError> {
    if path.exists() {
        std::fs::copy(path, path.with_extension("vdf.bak"))?;
    }
    crate::install::write_atomic(path, &vdf::write_binary(value))?;
    Ok(())
}

/// Grid file names: <appid><suffix>.png in userdata/<id>/config/grid.
const PICTURES: [(&str, &str); 4] = [("wide", ""), ("portrait", "p"), ("hero", "_hero"), ("logo", "_logo")];

/// Adds or updates the entry for every Steam user. Returns its appid.
pub fn add(steam_root: &Path, game: &Game, e: &Entry, dir: &Path) -> Result<u32, LaunchError> {
    if steam::steam_running() {
        return Err(LaunchError::SteamRunning("the Steam library"));
    }
    let mut id = 0;
    for user in steam::users(steam_root) {
        let path = shortcuts_file(&user);
        let (next, appid) = upsert(&read_file(&path)?, e);
        write_file(&path, &next)?;
        id = appid;
        let art = &game.module.art;
        for (slot, suffix) in PICTURES {
            let src = match slot {
                "wide" => &art.wide,
                "portrait" => &art.portrait,
                "hero" => &art.hero,
                _ => &art.logo,
            };
            if let Some(src) = src.as_deref().map(|s| dir.join(s)).filter(|p| p.is_file()) {
                let grid = user.join("config/grid");
                std::fs::create_dir_all(&grid)?;
                std::fs::copy(src, grid.join(format!("{appid}{suffix}.png")))?;
            }
        }
    }
    Ok(id)
}

/// Takes the entry out for every Steam user.
pub fn remove_all(steam_root: &Path, name: &str) -> Result<(), LaunchError> {
    if steam::steam_running() {
        return Err(LaunchError::SteamRunning("the Steam library"));
    }
    for user in steam::users(steam_root) {
        let path = shortcuts_file(&user);
        if path.exists() {
            let file = read_file(&path)?;
            write_file(&path, &remove(&file, name))?;
        }
    }
    Ok(())
}

/// Whether any Steam user has an entry with this name.
pub fn present(steam_root: &Path, name: &str) -> bool {
    steam::users(steam_root).iter().any(|u| {
        read_file(&shortcuts_file(u)).ok().is_some_and(|f| match f.get("shortcuts") {
            Some(Bin::Map(items)) => items.iter().any(|(_, v)| v.get("appname").and_then(Bin::as_str) == Some(name)),
            _ => false,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            exe: "\"/x/play.sh\"".into(),
            start_dir: "\"/x\"".into(),
            launch_options: String::new(),
            icon: String::new(),
        }
    }

    #[test]
    fn appid_matches_steam_formula() {
        // The SF4 Deck installer computes crc32(quoted exe + name) | 0x80000000 too.
        let exe = "\"/home/deck/.local/share/Steam/steamapps/compatdata/45760/pfx/drive_c/Ember/ember-play.sh\"";
        let id = shortcut_appid(exe, "SF4 Ember Netplay");
        assert_eq!(id, crc32fast::hash(format!("{exe}SF4 Ember Netplay").as_bytes()) | 0x8000_0000);
        assert!(id & 0x8000_0000 != 0);
        assert_eq!(rungameid(id) >> 32, id as u64);
    }

    #[test]
    fn upsert_adds_once_and_keeps_other_shortcuts() {
        let other = Bin::Map(vec![(
            "shortcuts".into(),
            Bin::Map(vec![("0".into(), Bin::Map(vec![("AppName".into(), Bin::Str("Emu".into()))]))]),
        )]);
        let (once, id) = upsert(&other, &entry("SFxT Ember Netplay"));
        let (twice, id2) = upsert(&once, &entry("SFxT Ember Netplay"));
        assert_eq!(id, id2);
        assert_eq!(once, twice);
        let Some(Bin::Map(items)) = twice.get("shortcuts") else { panic!() };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].0, "1");
        assert_eq!(items[1].1.get("appid").and_then(Bin::as_int), Some(id as i32));
        // Round trip through the real file format.
        assert_eq!(vdf::parse_binary(&vdf::write_binary(&twice)).unwrap(), twice);

        let gone = remove(&twice, "SFxT Ember Netplay");
        let Some(Bin::Map(items)) = gone.get("shortcuts") else { panic!() };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].1.get("appname").and_then(Bin::as_str), Some("Emu"));
    }

    #[test]
    fn upsert_on_an_empty_file() {
        let (v, _) = upsert(&vdf::parse_binary(&[]).unwrap(), &entry("A"));
        let Some(Bin::Map(items)) = v.get("shortcuts") else { panic!() };
        assert_eq!(items[0].0, "0");
    }
}
