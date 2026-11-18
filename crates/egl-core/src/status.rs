//! What the library shows for each game.

use crate::catalog::{Catalog, Game};
use crate::install::{Installed, State};
use crate::steam::{self, GameInstall};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatus {
    pub id: String,
    pub name: String,
    pub short: String,
    pub mod_name: String,
    pub blurb: String,
    pub game: Option<GameInstall>,
    /// Which Steam version was found ("GFWL", "Steam"), when the game has several.
    pub version_label: Option<String>,
    pub installed: Option<Installed>,
    pub mod_dir: Option<PathBuf>,
    pub channel: String,
    pub channels: Vec<ChannelInfo>,
    pub steam_entry: bool,
    /// Linux: the game's launch option has the wrapper, so -ember works.
    pub launch_ready: bool,
    /// Linux: the game's Proton prefix exists (the game ran once).
    pub prefix_ready: bool,
}

pub fn channel_for(game: &Game, state: &State) -> String {
    let wanted = state.mods.get(&game.id).map(|m| m.channel.clone()).or_else(|| state.channels.get(&game.id).cloned());
    match wanted {
        Some(c) if game.module.channel(&c).is_some() => c,
        _ => game.module.channels[0].id.clone(),
    }
}

pub fn locate(game: &Game, state: &State, roots: &[PathBuf]) -> Option<GameInstall> {
    if let Some(dir) = state.manual_folders.get(&game.id) {
        if let Some(g) = steam::from_folder(dir, &game.steam) {
            return Some(g);
        }
    }
    steam::find_game(roots, &game.steam)
}

pub fn scan(catalog: &Catalog, state: &State, data: &Path) -> Vec<GameStatus> {
    let roots = steam::steam_roots();
    let steam_root = roots.first();
    let wrapper = data.join("steam-wrapper.sh");
    catalog
        .games
        .iter()
        .map(|g| {
            let found = locate(g, state, &roots);
            let installed = state.mods.get(&g.id).filter(|m| m.dir.join(&g.module.launcher).is_file()).cloned();
            let version_label = found.as_ref().and_then(|f| {
                (g.steam.len() > 1)
                    .then(|| g.steam.iter().find(|a| a.appid == f.appid).and_then(|a| a.label.clone()))
                    .flatten()
            });
            let prefix_ready =
                cfg!(windows) || found.as_ref().is_some_and(|f| steam::proton_prefix(f).join("drive_c").is_dir());
            let launch_ready = cfg!(windows)
                || match (steam_root, &found) {
                    (Some(root), Some(f)) => crate::launch::launch_option_ready(root, f.appid, &wrapper),
                    _ => false,
                };
            GameStatus {
                id: g.id.clone(),
                name: g.name.clone(),
                short: g.short.clone(),
                mod_name: g.module.name.clone(),
                blurb: g.module.blurb.clone(),
                mod_dir: found.as_ref().map(|f| crate::launch::mod_dir(data, g, f)),
                version_label,
                steam_entry: steam_root.is_some_and(|r| crate::shortcuts::present(r, &g.module.name)),
                channel: channel_for(g, state),
                channels: g
                    .module
                    .channels
                    .iter()
                    .map(|c| ChannelInfo { id: c.id.clone(), label: c.label.clone() })
                    .collect(),
                installed,
                launch_ready,
                prefix_ready,
                game: found,
            }
        })
        .collect()
}
