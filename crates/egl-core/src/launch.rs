//! Starting a mod's own launcher. On Windows the launcher is started directly.
//! On Linux it must run inside the game's Proton session, so (as Ember's
//! Steam Deck installers do) a wrapper goes in front of the game's Steam
//! launch option and swaps the game's exe for the mod's launcher whenever
//! the game is started with -ember. Without -ember the game starts plain.

use crate::catalog::Game;
use crate::steam::{self, GameInstall};
use crate::vdf;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the mod is not installed")]
    NotInstalled,
    #[error("Steam must be closed to change {0}")]
    SteamRunning(&'static str),
    #[error("{0} has a character Steam's launch line cannot carry safely")]
    UnsafePath(String),
    #[error("Steam file {0}: {1}")]
    Vdf(String, vdf::VdfError),
    #[error("start the game once from Steam first, so Proton makes its prefix")]
    NoPrefix,
}

/// Where the mod lives. Windows: the launcher's own data folder. Linux: inside
/// the game's Proton prefix (Ember's packaging refuses Z:\ paths there).
pub fn mod_dir(data: &Path, game: &Game, install: &GameInstall) -> PathBuf {
    if cfg!(windows) {
        data.join("mods").join(&game.module.folder)
    } else {
        steam::proton_prefix(install).join("drive_c/EmberGames").join(&game.module.folder)
    }
}

/// The arguments the mod's launcher gets on Windows.
pub fn windows_args(game: &Game, install: &GameInstall) -> Vec<String> {
    match &game.module.game_arg {
        Some(arg) => vec![arg.clone(), install.dir.to_string_lossy().into_owned()],
        None => Vec::new(),
    }
}

/// Starts the mod. Windows: its launcher, in its folder. Linux: Steam starts
/// the game with -ember, and the wrapper runs the mod's launcher instead.
pub fn start(game: &Game, install: &GameInstall, dir: &Path) -> Result<(), LaunchError> {
    let launcher = dir.join(&game.module.launcher);
    if !launcher.is_file() {
        return Err(LaunchError::NotInstalled);
    }
    if cfg!(windows) {
        let mut cmd = std::process::Command::new(&launcher);
        cmd.args(windows_args(game, install)).current_dir(dir);
        cmd.spawn()?;
    } else {
        std::process::Command::new("steam").args(["-applaunch", &install.appid.to_string(), "-ember"]).spawn()?;
    }
    Ok(())
}

pub const WRAPPER: &str = r#"#!/bin/bash
# Ember Games Launcher: put in front of a game's Steam launch option as
#   "<this file>" %command%
# With -ember in the command, the game's exe is swapped for the Ember launcher
# named in launch/<appid>.conf, inside the game's own Proton session. Without
# -ember (the game's own Play button) the plain game starts.
[[ " $* " == *" -ember "* ]] || exec "$@"
here=$(dirname "$(readlink -f "$0")")
conf="$here/launch/${SteamAppId:-${STEAM_COMPAT_APP_ID:-none}}.conf"
args=()
for a in "$@"; do [ "$a" = -ember ] || args+=("$a"); done
[ -f "$conf" ] || { echo "ember-games: no $conf; starting the plain game" >&2; exec "${args[@]}"; }
exe= launcher= gamearg=
while IFS='=' read -r k v; do
  case "$k" in GAME_EXE) exe=$v ;; LAUNCHER) launcher=$v ;; GAME_ARG) gamearg=$v ;; esac
done < "$conf"
[ -f "$launcher" ] || { echo "ember-games: $launcher is gone; starting the plain game" >&2; exec "${args[@]}"; }
# Ember's launchers look for the game where Steam says it is.
if [ -n "$STEAM_COMPAT_INSTALL_PATH" ]; then
  win="Z:${STEAM_COMPAT_INSTALL_PATH//\//\\}"
  export STEAM_APP_PATH="$win"
fi
shopt -s nocasematch
out=()
for a in "${args[@]}"; do
  if [[ -n $exe && $a == *"$exe" ]]; then
    out+=("$launcher")
    [ -n "$gamearg" ] && [ -n "$win" ] && out+=("$gamearg" "$win")
  else
    out+=("$a")
  fi
done
exec "${out[@]}"
"#;

fn check_safe(p: &str) -> Result<(), LaunchError> {
    if p.contains(['"', '\'', '`', '$', '\\', '\n', '\r', '=']) && !cfg!(windows) {
        return Err(LaunchError::UnsafePath(p.into()));
    }
    if p.contains(['"', '\n', '\r']) {
        return Err(LaunchError::UnsafePath(p.into()));
    }
    Ok(())
}

/// Writes the wrapper and this game's launch/<appid>.conf. Returns the
/// wrapper's path.
pub fn write_wrapper(data: &Path, game: &Game, install: &GameInstall, dir: &Path) -> Result<PathBuf, LaunchError> {
    let wrapper = data.join("steam-wrapper.sh");
    check_safe(&wrapper.to_string_lossy())?;
    let launcher = dir.join(&game.module.launcher);
    check_safe(&launcher.to_string_lossy())?;
    crate::install::write_atomic(&wrapper, WRAPPER.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    }
    let app = game.steam.iter().find(|a| a.appid == install.appid).unwrap_or(&game.steam[0]);
    let exe = Path::new(&app.exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let conf = format!(
        "GAME_EXE={exe}\nLAUNCHER={}\nGAME_ARG={}\n",
        launcher.display(),
        game.module.game_arg.clone().unwrap_or_default()
    );
    crate::install::write_atomic(&data.join(format!("launch/{}.conf", install.appid)), conf.as_bytes())?;
    Ok(wrapper)
}

/// The launch option with the wrapper in front of %command% (kept as it was
/// when the wrapper is already there).
pub fn with_wrapper(old: &str, wrapper: &str) -> String {
    let quoted = format!("\"{wrapper}\"");
    if old.contains(&quoted) {
        return old.to_string();
    }
    let old = old.trim();
    if old.is_empty() {
        format!("{quoted} %command%")
    } else if old.contains("%command%") {
        old.replacen("%command%", &format!("{quoted} %command%"), 1)
    } else {
        // Plain words without %command% are arguments for the game.
        format!("{quoted} %command% {old}")
    }
}

/// The launch option with the wrapper taken out again.
pub fn without_wrapper(old: &str, wrapper: &str) -> String {
    let quoted = format!("\"{wrapper}\" ");
    let out = old.replacen(&quoted, "", 1);
    if out.trim() == "%command%" {
        String::new()
    } else {
        out
    }
}

/// Puts the wrapper in (or takes it out of) the game's launch option for
/// every Steam user. Steam must be closed.
pub fn set_launch_option(steam_root: &Path, appid: u32, wrapper: &Path, on: bool) -> Result<usize, LaunchError> {
    if steam::steam_running() {
        return Err(LaunchError::SteamRunning("launch options"));
    }
    let wrapper = wrapper.to_string_lossy();
    let mut changed = 0;
    for user in steam::users(steam_root) {
        let path = user.join("config/localconfig.vdf");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut root = vdf::parse_text(&text).map_err(|e| LaunchError::Vdf(path.display().to_string(), e))?;
        let app = root.child_path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &appid.to_string()]);
        let old = app.str("LaunchOptions").unwrap_or("").to_string();
        let new = if on { with_wrapper(&old, &wrapper) } else { without_wrapper(&old, &wrapper) };
        if new != old {
            app.set("LaunchOptions", &new);
            std::fs::copy(&path, path.with_extension("vdf.bak"))?;
            crate::install::write_atomic(&path, vdf::format_text(&root).as_bytes())?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Whether every user's launch option for the game has the wrapper.
pub fn launch_option_ready(steam_root: &Path, appid: u32, wrapper: &Path) -> bool {
    let quoted = format!("\"{}\"", wrapper.to_string_lossy());
    let users = steam::users(steam_root);
    !users.is_empty()
        && users.iter().all(|u| {
            std::fs::read_to_string(u.join("config/localconfig.vdf"))
                .ok()
                .and_then(|t| vdf::parse_text(&t).ok())
                .and_then(|r| {
                    r.path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &appid.to_string()])
                        .and_then(|a| a.str("LaunchOptions"))
                        .map(|o| o.contains(&quoted))
                })
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_goes_in_front_of_command_and_comes_out_again() {
        let w = "/home/p/.local/share/ember-games/steam-wrapper.sh";
        let q = format!("\"{w}\"");
        assert_eq!(with_wrapper("", w), format!("{q} %command%"));
        assert_eq!(with_wrapper("PROTON_LOG=1 %command% -x", w), format!("PROTON_LOG=1 {q} %command% -x"));
        assert_eq!(with_wrapper("-windowed", w), format!("{q} %command% -windowed"));
        let once = with_wrapper("gamemoderun %command%", w);
        assert_eq!(with_wrapper(&once, w), once);
        assert_eq!(without_wrapper(&once, w), "gamemoderun %command%");
        assert_eq!(without_wrapper(&with_wrapper("", w), w), "");
    }

    #[cfg(unix)]
    #[test]
    fn launch_option_edit_keeps_the_rest_of_localconfig() {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("userdata/123/config");
        std::fs::create_dir_all(&cfg).unwrap();
        let text = "\"UserLocalConfigStore\"\n{\n\t\"friends\"\t\t\"x\"\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"357190\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        std::fs::write(cfg.join("localconfig.vdf"), text).unwrap();
        let w = t.path().join("w.sh");
        if steam::steam_running() {
            return; // the dev box's own Steam; the edit is refused, as it should be
        }
        assert_eq!(set_launch_option(t.path(), 357190, &w, true).unwrap(), 1);
        assert!(launch_option_ready(t.path(), 357190, &w));
        let after = vdf::parse_text(&std::fs::read_to_string(cfg.join("localconfig.vdf")).unwrap()).unwrap();
        assert_eq!(after.map("UserLocalConfigStore").unwrap().str("friends"), Some("x"));
        let app = after.path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", "357190"]).unwrap();
        assert_eq!(app.str("LastPlayed"), Some("1"));
        assert!(cfg.join("localconfig.vdf.bak").exists());
        assert_eq!(set_launch_option(t.path(), 357190, &w, true).unwrap(), 0);
        set_launch_option(t.path(), 357190, &w, false).unwrap();
        assert!(!launch_option_ready(t.path(), 357190, &w));
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_script_swaps_the_exe_only_with_ember() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path().join("steam-wrapper.sh");
        std::fs::write(&w, WRAPPER).unwrap();
        std::fs::create_dir_all(t.path().join("launch")).unwrap();
        let launcher = t.path().join("pfx/umvc3-ember-launcher.exe");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "").unwrap();
        std::fs::write(
            t.path().join("launch/357190.conf"),
            format!("GAME_EXE=umvc3.exe\nLAUNCHER={}\nGAME_ARG=--game\n", launcher.display()),
        )
        .unwrap();
        let run = |args: &[&str]| {
            let o = std::process::Command::new("bash")
                .arg(&w)
                .args(["printf", "%s|"])
                .args(args)
                .env("SteamAppId", "357190")
                .env("STEAM_COMPAT_INSTALL_PATH", "/lib/common/UMVC3")
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap()
        };
        assert_eq!(
            run(&["proton", "waitforexitandrun", "/lib/common/UMVC3/umvc3.exe"]),
            "proton|waitforexitandrun|/lib/common/UMVC3/umvc3.exe|"
        );
        assert_eq!(
            run(&["proton", "waitforexitandrun", "/lib/common/UMVC3/umvc3.exe", "-ember"]),
            format!("proton|waitforexitandrun|{}|--game|Z:\\lib\\common\\UMVC3|", launcher.display())
        );
    }
}
