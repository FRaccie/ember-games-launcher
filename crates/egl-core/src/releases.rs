//! Picking a channel's newest release from GitHub's release list, and the
//! checksum it must match.

use crate::catalog::Channel;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    /// "sha256:<hex>", which GitHub computes for every upload.
    #[serde(default)]
    pub digest: Option<String>,
}

/// A release the launcher can install.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub tag: String,
    pub title: String,
    pub published: String,
    pub notes: String,
    pub page: String,
    pub asset: String,
    pub url: String,
    pub size: u64,
    /// GitHub's sha256 of the asset, lowercase hex.
    pub sha256: Option<String>,
    /// A checksum file published beside it (name.sha256 or *SHA256SUMS*).
    pub checksum_url: Option<String>,
}

fn hex_digest(d: &str) -> Option<String> {
    let h = d.strip_prefix("sha256:")?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// The newest release in the list that belongs to the channel and carries its
/// package. GitHub lists newest first; published dates break any doubt.
pub fn pick(channel: &Channel, list: &[GhRelease]) -> Option<Release> {
    let tag = Regex::new(&channel.tag).ok()?;
    let asset = Regex::new(&channel.asset).ok()?;
    let mut best: Option<(&GhRelease, &GhAsset)> = None;
    for r in list {
        if r.draft || (r.prerelease && !channel.prerelease) || !tag.is_match(&r.tag_name) {
            continue;
        }
        let Some(a) = r.assets.iter().find(|a| asset.is_match(&a.name)) else { continue };
        let newer = match best {
            None => true,
            Some((b, _)) => r.published_at.as_deref().unwrap_or("") > b.published_at.as_deref().unwrap_or(""),
        };
        if newer {
            best = Some((r, a));
        }
    }
    let (r, a) = best?;
    let sums = r
        .assets
        .iter()
        .find(|s| s.name == format!("{}.sha256", a.name) || s.name.to_ascii_uppercase().contains("SHA256SUMS"));
    Some(Release {
        tag: r.tag_name.clone(),
        title: r.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| r.tag_name.clone()),
        published: r.published_at.clone().unwrap_or_default(),
        notes: r.body.clone().unwrap_or_default(),
        page: r.html_url.clone(),
        asset: a.name.clone(),
        url: a.browser_download_url.clone(),
        size: a.size,
        sha256: a.digest.as_deref().and_then(hex_digest),
        checksum_url: sums.map(|s| s.browser_download_url.clone()),
    })
}

/// The hash a checksum file gives for `asset`: "<hex>  name" lines, or a
/// single bare hash. Letter case of the hex does not matter.
pub fn checksum_for(text: &str, asset: &str) -> Option<String> {
    let mut lone = None;
    let mut lines = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        lines += 1;
        let mut parts = line.split_whitespace();
        let hash = parts.next()?.to_ascii_lowercase();
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        match parts.next() {
            Some(name) if name.trim_start_matches('*') == asset => return Some(hash),
            None => lone = Some(hash),
            _ => {}
        }
    }
    if lines == 1 {
        lone
    } else {
        None
    }
}

pub fn api_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases?per_page=30")
}

/// Only GitHub's own hosts are ever downloaded from.
pub fn allowed_url(url: &str) -> bool {
    [
        "https://github.com/",
        "https://api.github.com/",
        "https://objects.githubusercontent.com/",
        "https://release-assets.githubusercontent.com/",
        "https://raw.githubusercontent.com/",
    ]
    .iter()
    .any(|p| url.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, at: &str, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            name: Some(format!("R {tag}")),
            draft: false,
            prerelease: pre,
            published_at: Some(at.into()),
            body: None,
            html_url: String::new(),
            assets: assets
                .iter()
                .map(|n| GhAsset {
                    name: n.to_string(),
                    size: 1,
                    browser_download_url: format!("https://github.com/x/{n}"),
                    digest: Some(format!("sha256:{}", "AB".repeat(32))),
                })
                .collect(),
        }
    }

    fn channel(pre: bool, tag: &str, asset: &str) -> Channel {
        Channel {
            id: "c".into(),
            label: "C".into(),
            repo: "o/r".into(),
            prerelease: pre,
            tag: tag.into(),
            asset: asset.into(),
        }
    }

    #[test]
    fn nightly_skips_non_build_tags_and_picks_newest() {
        let list = vec![
            rel("voice-packs", true, "2026-10-20", &["packs.zip"]),
            rel("steamdeck", true, "2026-11-30", &["install.desktop"]),
            rel(
                "v1.6.1-nightly20261116",
                true,
                "2026-11-16",
                &["sf4-ember-netplay-1.6.1-nightly20261116.zip", "sf4-ember-netplay-1.6.1-nightly20261116.zip.sha256"],
            ),
            rel("v1.6.0-nightly20261115.2", true, "2026-11-15", &["sf4-ember-netplay-1.6.0-nightly20261115.2.zip"]),
        ];
        let ch = channel(true, r"^v\d+\.\d+\.\d+-nightly[0-9.]+$", r"^sf4-ember-netplay-.*-nightly[0-9.]+\.zip$");
        let r = pick(&ch, &list).unwrap();
        assert_eq!(r.tag, "v1.6.1-nightly20261116");
        assert_eq!(r.sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert!(r.checksum_url.unwrap().ends_with(".zip.sha256"));
    }

    #[test]
    fn stable_ignores_prereleases_and_drafts() {
        let mut draft = rel("v1.7.0", false, "2026-12-01", &["m-1.7.0.zip"]);
        draft.draft = true;
        let list = vec![
            draft,
            rel("v1.6.2-rc1", true, "2026-11-20", &["m-1.6.2.zip"]),
            rel("v1.6.1", false, "2026-11-17", &["m-1.6.1.zip"]),
        ];
        let r = pick(&channel(false, r"^v[\d.]+(-rc\d+)?$", r"^m-[\d.]+\.zip$"), &list).unwrap();
        assert_eq!(r.tag, "v1.6.1");
        assert!(pick(&channel(false, r"^v[\d.]+$", r"^nothing$"), &list).is_none());
    }

    #[test]
    fn checksum_files() {
        let h = "DEFA3EC57D77037AFAE659C4020BDF40628099E44D455D05383B20CC6219D7F6";
        let sums = format!("{h}  sfxt-setup.exe\n{}  sfxt.zip\n", "80".repeat(32));
        assert_eq!(checksum_for(&sums, "sfxt.zip").as_deref(), Some("80".repeat(32).as_str()));
        assert_eq!(checksum_for(&sums, "other.zip"), None);
        assert_eq!(checksum_for(&format!("{h}\n"), "x.zip"), Some(h.to_ascii_lowercase()));
        assert_eq!(checksum_for(&format!("{h} *a.zip"), "a.zip"), Some(h.to_ascii_lowercase()));
    }

    #[test]
    fn only_github_hosts() {
        assert!(allowed_url("https://github.com/FRaccie/SFxT-Ember/releases/download/v1/x.zip"));
        assert!(!allowed_url("http://github.com/x"));
        assert!(!allowed_url("https://github.com.evil.example/x"));
    }
}
