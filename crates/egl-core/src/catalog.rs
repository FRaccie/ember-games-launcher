//! The catalog: which games Ember supports, which mod goes with each and
//! where its releases come from. A copy ships in the app; a newer one is
//! fetched from the launcher's repo when it can be reached.

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const BUNDLED: &str = include_str!("../../../catalog/catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub schema: u32,
    pub games: Vec<Game>,
    #[serde(default)]
    pub reports: Reports,
}

/// Where log bundles go. No url: bundles are only saved to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reports {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub name: String,
    pub short: String,
    /// Steam apps that are this game, the one most players use first.
    pub steam: Vec<SteamApp>,
    #[serde(rename = "mod")]
    pub module: Mod,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SteamApp {
    pub appid: u32,
    /// The game exe, relative to the game folder ('/' separated).
    pub exe: String,
    /// Usual folder names under steamapps/common.
    pub folders: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mod {
    pub name: String,
    #[serde(default)]
    pub blurb: String,
    /// Folder name the mod is installed in (never the game's folder).
    pub folder: String,
    /// The mod's own launcher, relative to its folder.
    pub launcher: String,
    /// The launcher's option that names the game folder, when it has one.
    #[serde(default)]
    pub game_arg: Option<String>,
    #[serde(default)]
    pub art: Art,
    pub channels: Vec<Channel>,
    /// Where the mod writes its logs.
    #[serde(default)]
    pub logs: Vec<LogSource>,
}

/// A folder of logs. dir starts with {appdata}, {localappdata} or {mod};
/// files are name patterns (no folders).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogSource {
    pub dir: String,
    pub files: Vec<String>,
}

/// Pictures inside the mod package, for its Steam entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Art {
    pub icon: Option<String>,
    pub wide: Option<String>,
    pub portrait: Option<String>,
    pub hero: Option<String>,
    pub logo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub label: String,
    /// owner/name on GitHub.
    pub repo: String,
    /// Whether pre-releases count for this channel.
    pub prerelease: bool,
    /// Tags that belong to the channel (others, like "steamdeck", are skipped).
    pub tag: String,
    /// The package asset's name.
    pub asset: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog schema {0} is newer than this launcher reads ({SCHEMA}); update the launcher")]
    Schema(u32),
    #[error("catalog: {0}")]
    Invalid(String),
}

fn safe_relative(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with(['/', '\\'])
        && !p.contains(':')
        && p.split(['/', '\\']).all(|s| !s.is_empty() && s != "..")
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Catalog, CatalogError> {
        let c: Catalog = serde_json::from_str(text)?;
        if c.schema > SCHEMA {
            return Err(CatalogError::Schema(c.schema));
        }
        let bad = |m: String| Err(CatalogError::Invalid(m));
        let mut ids = std::collections::HashSet::new();
        for g in &c.games {
            if !ids.insert(&g.id) || g.id.is_empty() || !g.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return bad(format!("game id '{}' is empty, repeated or not plain", g.id));
            }
            if g.steam.is_empty() {
                return bad(format!("{}: no Steam app", g.id));
            }
            for a in &g.steam {
                if !safe_relative(&a.exe) || a.folders.iter().any(|f| !safe_relative(f) || f.contains(['/', '\\'])) {
                    return bad(format!("{}: unsafe exe or folder", g.id));
                }
            }
            let m = &g.module;
            if !safe_relative(&m.folder) || m.folder.contains(['/', '\\']) || !safe_relative(&m.launcher) {
                return bad(format!("{}: unsafe mod folder or launcher", g.id));
            }
            let art = [&m.art.icon, &m.art.wide, &m.art.portrait, &m.art.hero, &m.art.logo];
            if art.iter().any(|a| a.as_deref().is_some_and(|a| !safe_relative(a))) {
                return bad(format!("{}: unsafe art path", g.id));
            }
            for l in &m.logs {
                let rest = ["{appdata}", "{localappdata}", "{mod}"].iter().find_map(|t| l.dir.strip_prefix(t));
                let ok = rest.is_some_and(|r| r.is_empty() || (r.starts_with('/') && safe_relative(&r[1..])))
                    && !l.files.is_empty()
                    && l.files.iter().all(|f| !f.is_empty() && !f.contains(['/', '\\']) && !f.contains(".."));
                if !ok {
                    return bad(format!("{}: unsafe log source {}", g.id, l.dir));
                }
            }
            if m.channels.is_empty() {
                return bad(format!("{}: no channel", g.id));
            }
            for ch in &m.channels {
                let repo_ok = ch.repo.split('/').count() == 2
                    && ch
                        .repo
                        .split('/')
                        .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)));
                if !repo_ok {
                    return bad(format!("{}: bad repo '{}'", g.id, ch.repo));
                }
                for re in [&ch.tag, &ch.asset] {
                    if let Err(e) = Regex::new(re) {
                        return bad(format!("{}/{}: bad pattern: {e}", g.id, ch.id));
                    }
                }
            }
        }
        Ok(c)
    }

    pub fn bundled() -> Catalog {
        Catalog::parse(BUNDLED).expect("bundled catalog is checked by tests")
    }

    pub fn game(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }
}

impl Mod {
    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_is_valid_and_has_the_four_games() {
        let c = Catalog::bundled();
        let ids: Vec<_> = c.games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["usf4", "sfxt", "umvc3", "sfv"]);
        let sfxt = c.game("sfxt").unwrap();
        assert_eq!(sfxt.steam.iter().map(|a| a.appid).collect::<Vec<_>>(), [55550, 209120]);
        assert_eq!(c.game("usf4").unwrap().module.channels.len(), 2);
        assert!(c.games.iter().all(|g| !g.module.logs.is_empty()));
    }

    #[test]
    fn bundled_patterns_match_real_release_names() {
        let c = Catalog::bundled();
        let check = |game: &str, ch: &str, tag: &str, asset: &str| {
            let ch = c.game(game).unwrap().module.channel(ch).unwrap();
            assert!(Regex::new(&ch.tag).unwrap().is_match(tag), "{tag}");
            assert!(Regex::new(&ch.asset).unwrap().is_match(asset), "{asset}");
        };
        check("usf4", "stable", "v1.6.1", "sf4-ember-netplay-1.6.1.zip");
        check("usf4", "nightly", "v1.6.1-nightly20261116", "sf4-ember-netplay-1.6.1-nightly20261116.zip");
        check("sfxt", "beta", "v1.0.0-beta.12", "sfxt-ember-1.0.0-beta.12.zip");
        check(
            "umvc3",
            "nightly",
            "v1.0.0-nightly.20261118.1427.3a50ac2",
            "umvc3-ember-1.0.0-nightly.20261118.1427.3a50ac2.zip",
        );
        check("sfv", "stable", "v0.1.0", "sfv-ember-0.1.0.zip");
        check("sfv", "beta", "v0.1.0-beta.10", "sfv-ember-0.1.0-beta.10.zip");

        let nightly = c.game("usf4").unwrap().module.channel("nightly").unwrap();
        for not_a_build in ["steamdeck", "voice-packs"] {
            assert!(!Regex::new(&nightly.tag).unwrap().is_match(not_a_build));
        }
        let stable = c.game("usf4").unwrap().module.channel("stable").unwrap();
        assert!(!Regex::new(&stable.asset).unwrap().is_match("upgrade-sf4-ember-netplay-1.6.0-to-1.6.1.zip"));
        assert!(!Regex::new(&stable.asset).unwrap().is_match("sf4-ember-netplay-1.6.1.zip.sha256"));
    }

    #[test]
    fn refuses_unsafe_or_newer_catalogs() {
        let base = Catalog::bundled();
        let mut c = base.clone();
        c.games[0].module.folder = "../escape".into();
        assert!(Catalog::parse(&serde_json::to_string(&c).unwrap()).is_err());
        let mut c = base.clone();
        c.games[0].module.launcher = "C:/Windows/x.exe".into();
        assert!(Catalog::parse(&serde_json::to_string(&c).unwrap()).is_err());
        let mut c = base.clone();
        c.games[1].id = "usf4".into();
        assert!(Catalog::parse(&serde_json::to_string(&c).unwrap()).is_err());
        let mut c = base.clone();
        c.games[0].module.logs[0].dir = "{appdata}/../../x".into();
        assert!(Catalog::parse(&serde_json::to_string(&c).unwrap()).is_err());
        let mut c = base.clone();
        c.games[0].module.logs[0].dir = "/etc".into();
        assert!(Catalog::parse(&serde_json::to_string(&c).unwrap()).is_err());
        let mut c = base;
        c.schema = SCHEMA + 1;
        assert!(matches!(Catalog::parse(&serde_json::to_string(&c).unwrap()), Err(CatalogError::Schema(_))));
    }
}
