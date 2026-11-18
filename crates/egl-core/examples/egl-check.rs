//! Read-only checks against the real world, for development:
//!   egl-check releases   newest release per catalog channel, from GitHub
//!   egl-check scan       which games this PC has, and where their logs are
//!   egl-check unpack     install_zip on a real package (writes only <dest>)
//! Nothing is written or installed.

use egl_core::catalog::Catalog;
use egl_core::{install, logs, releases, status, steam};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let catalog = Catalog::bundled();
    match std::env::args().nth(1).as_deref() {
        Some("releases") => {
            let http = install::client();
            let mut failed = false;
            for g in &catalog.games {
                for ch in &g.module.channels {
                    let text = install::fetch_text(&http, &releases::api_url(&ch.repo)).await;
                    let list: Vec<releases::GhRelease> = match text.map(|t| serde_json::from_str(&t)) {
                        Ok(Ok(l)) => l,
                        other => {
                            println!("{:6} {:8} ERROR {:?}", g.id, ch.id, other.err());
                            failed = true;
                            continue;
                        }
                    };
                    match releases::pick(ch, &list) {
                        Some(r) => {
                            let sha = install::expected_sha256(&http, &r).await;
                            println!(
                                "{:6} {:8} {:42} {:55} {:>6.1} MB  sha256 {}",
                                g.id,
                                ch.id,
                                r.tag,
                                r.asset,
                                r.size as f64 / 1048576.0,
                                sha.as_deref().map(|s| &s[..12]).unwrap_or("MISSING")
                            );
                            failed |= sha.is_err();
                        }
                        None => {
                            println!("{:6} {:8} NO RELEASE MATCHES", g.id, ch.id);
                            failed = true;
                        }
                    }
                }
            }
            std::process::exit(failed as i32);
        }
        Some("scan") => {
            println!("steam roots: {:?}", steam::steam_roots());
            println!("steam running: {}", steam::steam_running());
            for r in steam::steam_roots() {
                println!("libraries of {}: {:?}", r.display(), steam::libraries(&r));
                println!("users: {}", steam::users(&r).len());
            }
            let data = install::data_dir();
            let state = install::State::load(&data.join("state.json"));
            for s in status::scan(&catalog, &state, &data) {
                let game = catalog.game(&s.id).unwrap();
                let found = s.game.as_ref();
                let logs = logs::find_logs(game, s.mod_dir.as_deref(), found);
                println!(
                    "{:6} game: {:60} app {:>6} {:5}  mod dir: {}  logs: {}  steam entry: {}",
                    s.id,
                    found.map(|f| f.dir.display().to_string()).unwrap_or_else(|| "not found".into()),
                    found.map(|f| f.appid).unwrap_or(0),
                    s.version_label.unwrap_or_default(),
                    s.mod_dir.map(|d| d.display().to_string()).unwrap_or_default(),
                    logs.len(),
                    s.steam_entry
                );
            }
        }
        Some("unpack") => {
            // egl-check unpack <zip> <dest> <launcher>: install_zip on a real package.
            let a: Vec<String> = std::env::args().skip(2).collect();
            let (zip, dest, launcher) = (std::path::Path::new(&a[0]), std::path::Path::new(&a[1]), &a[2]);
            match install::install_zip(zip, dest, launcher) {
                Ok(()) => println!("ok: {} has {}", dest.display(), launcher),
                Err(e) => {
                    println!("failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("bundle") => {
            // egl-check bundle <data dir>: a real log bundle of every game, into <data dir> only.
            let out = std::path::PathBuf::from(std::env::args().nth(2).expect("data dir"));
            let state = install::State::load(&install::data_dir().join("state.json"));
            let roots = steam::steam_roots();
            let sets: Vec<_> = catalog
                .games
                .iter()
                .map(|g| {
                    let found = status::locate(g, &state, &roots);
                    let dir = found.as_ref().map(|f| egl_core::launch::mod_dir(&out, g, f));
                    let files = logs::find_logs(g, dir.as_deref(), found.as_ref());
                    let info = logs::BundleGame {
                        id: g.id.clone(),
                        mod_tag: None,
                        channel: None,
                        appid: found.map(|f| f.appid),
                    };
                    logs::GameLogs { game: g, info, files }
                })
                .collect();
            match logs::make_bundle(&out, &sets) {
                Ok(Some(b)) => println!("bundle {} : {} parts, {} KB", b.path.display(), b.parts, b.size / 1024),
                Ok(None) => println!("nothing new"),
                Err(e) => println!("failed: {e}"),
            }
        }
        _ => eprintln!("usage: egl-check releases|scan|unpack <zip> <dest> <launcher>"),
    }
}
