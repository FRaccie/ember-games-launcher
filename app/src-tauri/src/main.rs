// Ember Games Launcher: the window and the commands the page calls. All the
// work is in egl-core; this file only holds state and turns results into
// messages the page shows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use egl_core::catalog::Catalog;
use egl_core::install::{self, Installed, State};
use egl_core::{edition, existing, launch, logs, package, releases, shortcuts, status, steam};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

const CATALOG_URL: &str = "https://raw.githubusercontent.com/FRaccie/ember-games-launcher/main/catalog/catalog.json";

struct App {
    data: PathBuf,
    catalog: Mutex<Catalog>,
    catalog_source: Mutex<String>,
    state: Mutex<State>,
    http: reqwest::Client,
    busy: Mutex<Vec<String>>,
}

impl App {
    fn state_path(&self) -> PathBuf {
        self.data.join("state.json")
    }
    fn save(&self, s: &State) -> Result<(), String> {
        s.save(&self.state_path()).map_err(|e| e.to_string())
    }
    fn game(&self, id: &str) -> Result<egl_core::catalog::Game, String> {
        self.catalog.lock().unwrap().game(id).cloned().ok_or_else(|| format!("unknown game {id}"))
    }
    fn remember(&self, id: &str, rec: Installed) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        s.mods.insert(id.to_string(), rec);
        let snapshot = s.clone();
        drop(s);
        self.save(&snapshot)
    }
    fn located(&self, id: &str) -> Result<(egl_core::catalog::Game, steam::GameInstall), String> {
        let game = self.game(id)?;
        let state = self.state.lock().unwrap().clone();
        let found = status::locate(&game, &state, &steam::steam_roots())
            .ok_or("the game was not found; pick its folder first")?;
        Ok((game, found))
    }
}

type Res<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    platform: &'static str,
    steam: Option<PathBuf>,
    steam_running: bool,
    catalog_source: String,
    reports_configured: bool,
    games: Vec<status::GameStatus>,
}

#[tauri::command]
fn overview(app: tauri::State<App>) -> Overview {
    let catalog = app.catalog.lock().unwrap().clone();
    let state = app.state.lock().unwrap().clone();
    Overview {
        platform: if cfg!(windows) { "windows" } else { "linux" },
        steam: steam::steam_roots().into_iter().next(),
        steam_running: steam::steam_running(),
        catalog_source: app.catalog_source.lock().unwrap().clone(),
        reports_configured: catalog.reports.url.is_some(),
        games: status::scan(&catalog, &state, &app.data),
    }
}

#[tauri::command]
async fn latest(app: tauri::State<'_, App>, id: String, channel: Option<String>) -> Res<releases::Release> {
    let game = app.game(&id)?;
    let channel = channel.unwrap_or_else(|| status::channel_for(&game, &app.state.lock().unwrap()));
    let ch = game.module.channel(&channel).ok_or("unknown channel")?.clone();
    let text = install::fetch_text(&app.http, &releases::api_url(&ch.repo)).await.map_err(|e| e.to_string())?;
    let list: Vec<releases::GhRelease> =
        serde_json::from_str(&text).map_err(|e| format!("GitHub answered oddly: {e}"))?;
    releases::pick(&ch, &list).ok_or_else(|| format!("no {} release with a package yet", ch.label))
}

#[tauri::command]
fn set_channel(app: tauri::State<App>, id: String, channel: String) -> Res<()> {
    let game = app.game(&id)?;
    game.module.channel(&channel).ok_or("unknown channel")?;
    let mut s = app.state.lock().unwrap();
    s.channels.insert(id.clone(), channel.clone());
    if let Some(m) = s.mods.get_mut(&id) {
        m.channel = channel;
    }
    let snapshot = s.clone();
    drop(s);
    app.save(&snapshot)
}

#[tauri::command]
fn set_folder(app: tauri::State<App>, id: String, folder: String) -> Res<steam::GameInstall> {
    let game = app.game(&id)?;
    let found = steam::from_folder(Path::new(&folder), &game.steam)
        .ok_or_else(|| format!("{} is not in that folder", game.steam[0].exe))?;
    let mut s = app.state.lock().unwrap();
    s.manual_folders.insert(id, found.dir.clone());
    let snapshot = s.clone();
    drop(s);
    app.save(&snapshot)?;
    Ok(found)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    id: String,
    step: usize,
    done: u64,
    total: u64,
}

struct BusyGuard<'a>(&'a App, String);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.lock().unwrap().retain(|b| b != &self.1);
    }
}

#[tauri::command]
async fn install_mod(window: tauri::Window, app: tauri::State<'_, App>, id: String) -> Res<Installed> {
    {
        let mut busy = app.busy.lock().unwrap();
        if busy.contains(&id) {
            return Err("already installing".into());
        }
        busy.push(id.clone());
    }
    let _guard = BusyGuard(&app, id.clone());
    let (game, found) = app.located(&id)?;
    let step = |n: usize, done: u64, total: u64| {
        let _ = window.emit("install-progress", Progress { id: id.clone(), step: n, done, total });
    };
    if !cfg!(windows) && !steam::proton_prefix(&found).join("drive_c").is_dir() {
        return Err(launch::LaunchError::NoPrefix.to_string());
    }
    step(0, 0, 0);
    let channel = status::channel_for(&game, &app.state.lock().unwrap());
    let release = latest(app.clone(), id.clone(), Some(channel.clone())).await?;
    let want = install::expected_sha256(&app.http, &release).await.map_err(|e| e.to_string())?;
    let zip = app.data.join("downloads").join(&release.asset);
    let have = zip.is_file() && install::sha256_file(&zip).map(|h| h == want).unwrap_or(false);
    if !have {
        step(1, 0, release.size);
        install::download(&app.http, &release, &want, &zip, |d, t| {
            step(1, d, t);
            true
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    step(2, 0, 0);
    let previous = app.state.lock().unwrap().mods.get(&id).cloned();
    let dir = previous.as_ref().map(|m| m.dir.clone()).unwrap_or_else(|| launch::mod_dir(&app.data, &game, &found));
    step(3, 0, 0);
    let (zip2, dir2, launcher) = (zip.clone(), dir.clone(), game.module.launcher.clone());
    tauri::async_runtime::spawn_blocking(move || install::install_zip(&zip2, &dir2, &launcher))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&zip);
    if !cfg!(windows) {
        launch::write_wrapper(&app.data, &game, &found, &dir).map_err(|e| e.to_string())?;
    }
    step(4, 0, 0);
    let rec = Installed {
        channel,
        tag: release.tag.clone(),
        launcher_crc: package::crc_of(&dir.join(&game.module.launcher)).ok(),
        dir,
        installed_at: install::now(),
        game_dir: found.dir.clone(),
        appid: found.appid,
        steam_shortcut: previous.as_ref().is_some_and(|m| m.steam_shortcut),
        adopted: previous.as_ref().is_some_and(|m| m.adopted),
    };
    app.remember(&id, rec.clone())?;
    Ok(rec)
}

/// GitHub's release list of every channel of the game, in catalog order.
async fn release_lists(
    app: &App,
    game: &egl_core::catalog::Game,
) -> Res<Vec<(egl_core::catalog::Channel, Vec<releases::GhRelease>)>> {
    let mut out = Vec::new();
    for ch in &game.module.channels {
        let text = install::fetch_text(&app.http, &releases::api_url(&ch.repo)).await.map_err(|e| e.to_string())?;
        let list = serde_json::from_str(&text).map_err(|e| format!("GitHub answered oddly: {e}"))?;
        out.push((ch.clone(), list));
    }
    Ok(out)
}

/// Takes over a copy the launcher found (installed by the mod's own setup,
/// from the zip or by a Deck installer): from now on it is updated and
/// checked in place. Only a folder the scan lists is accepted.
#[tauri::command]
async fn adopt_mod(app: tauri::State<'_, App>, id: String, dir: String) -> Res<Installed> {
    let (game, found) = app.located(&id)?;
    let roots = steam::steam_roots();
    let listed = existing::find(&game, Some(&found), roots.first().map(PathBuf::as_path), None);
    let copy = listed
        .into_iter()
        .find(|f| steam::same_folder(&f.dir, Path::new(&dir)))
        .ok_or("that folder is not a copy of the mod the launcher found")?;
    // The release it is, when GitHub answers; otherwise it is taken over as "unknown" and the next update fixes that.
    let lists = release_lists(&app, &game).await.unwrap_or_default();
    let known =
        existing::identify(&app.http, &game, &copy.dir, &lists, &app.data.join("packages")).await.ok().flatten();
    let channel = known
        .as_ref()
        .map(|(c, _)| c.clone())
        .unwrap_or_else(|| status::channel_for(&game, &app.state.lock().unwrap()));
    let tag = match (&known, &copy.version) {
        (Some((_, r)), _) => r.tag.clone(),
        (None, Some(v)) => format!("v{v}"),
        (None, None) => "unknown".into(),
    };
    if !cfg!(windows) {
        launch::write_wrapper(&app.data, &game, &found, &copy.dir).map_err(|e| e.to_string())?;
    }
    let rec = Installed {
        channel,
        tag,
        launcher_crc: package::crc_of(&copy.dir.join(&game.module.launcher)).ok(),
        dir: copy.dir,
        installed_at: install::now(),
        game_dir: found.dir.clone(),
        appid: found.appid,
        steam_shortcut: false,
        adopted: true,
    };
    app.remember(&id, rec.clone())?;
    Ok(rec)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckResult {
    /// The release the copy is, when one matches.
    tag: Option<String>,
    channel: Option<String>,
    check: Option<package::Check>,
}

/// Which release the managed copy is and whether its files are the
/// package's. A copy that matches no release is reported as such.
#[tauri::command]
async fn check_mod(app: tauri::State<'_, App>, id: String) -> Res<CheckResult> {
    let game = app.game(&id)?;
    let rec = app.state.lock().unwrap().mods.get(&id).cloned().ok_or("not installed")?;
    let lists = release_lists(&app, &game).await?;
    let cache = app.data.join("packages");
    let Some((channel, release)) =
        existing::identify(&app.http, &game, &rec.dir, &lists, &cache).await.map_err(|e| e.to_string())?
    else {
        return Ok(CheckResult { tag: None, channel: None, check: None });
    };
    let files = package::release_files(&app.http, &release, &cache).await.map_err(|e| e.to_string())?;
    let dir = rec.dir.clone();
    let check =
        tauri::async_runtime::spawn_blocking(move || package::check(&dir, &files)).await.map_err(|e| e.to_string())?;
    let mut now = rec.clone();
    now.tag = release.tag.clone();
    now.channel = channel.clone();
    if check.ok() {
        now.launcher_crc = package::crc_of(&rec.dir.join(&game.module.launcher)).ok();
    }
    app.remember(&id, now)?;
    Ok(CheckResult { tag: Some(release.tag), channel: Some(channel), check: Some(check) })
}

#[tauri::command]
fn uninstall_mod(app: tauri::State<App>, id: String) -> Res<()> {
    let rec = app.state.lock().unwrap().mods.get(&id).cloned().ok_or("not installed")?;
    let game = app.game(&id)?;
    // A copy the launcher took over belongs to its own setup: it is only forgotten.
    if !rec.adopted {
        // Only a folder the launcher made and that holds the mod's launcher.
        if !rec.dir.join(&game.module.launcher).is_file() || !rec.dir.ends_with(&game.module.folder) {
            return Err("that folder does not look like the mod's; leaving it".into());
        }
        std::fs::remove_dir_all(&rec.dir).map_err(|e| e.to_string())?;
    }
    let mut s = app.state.lock().unwrap();
    s.mods.remove(&id);
    let snapshot = s.clone();
    drop(s);
    app.save(&snapshot)
}

// ------------------------------------------------------------ game data (SFxT)

/// The manifest and the folders an old drop-in may have moved DLC to.
fn edition_setup(
    app: &App,
    id: &str,
) -> Res<(egl_core::catalog::Game, steam::GameInstall, edition::Manifest, Vec<PathBuf>)> {
    let (game, found) = app.located(id)?;
    let ed = game.module.edition.clone().ok_or("this game has no data check")?;
    let rec = app.state.lock().unwrap().mods.get(id).cloned();
    let rec = rec.ok_or_else(|| format!("install {} first: its patch manifest lists the files", game.module.name))?;
    let manifest = edition::Manifest::load(&rec.dir.join(&ed.manifest))
        .map_err(|e| format!("{e} ({} may be older than the patch)", game.module.name))?;
    let mut mod_dirs = vec![rec.dir.clone()];
    mod_dirs.extend(ed.drop_in_folders.iter().map(|f| found.dir.join(f)));
    Ok((game, found, manifest, mod_dirs))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditionView {
    label: String,
    report: edition::Report,
    /// The last patch of this game folder, if any.
    journal: Option<edition::Journal>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditionProgress {
    id: String,
    step: String,
    done: usize,
    total: usize,
}

/// Compares the game's files with every known version. `full` hashes them all.
#[tauri::command]
async fn edition_check(window: tauri::Window, app: tauri::State<'_, App>, id: String, full: bool) -> Res<EditionView> {
    let (game, found, manifest, mod_dirs) = edition_setup(&app, &id)?;
    let exe =
        steam::exe_in(&found.dir, &game.steam[0]).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    let exe = exe.unwrap_or_else(|| game.steam[0].exe.clone());
    let dir = found.dir.clone();
    let id2 = id.clone();
    let label = manifest.target_label().unwrap_or_else(|| game.module.edition.clone().unwrap().label);
    let report = tauri::async_runtime::spawn_blocking(move || {
        edition::validate(&dir, &exe, &manifest, &mod_dirs, full, |done, total| {
            if done % 50 == 0 {
                let _ = window
                    .emit("edition-progress", EditionProgress { id: id2.clone(), step: "check".into(), done, total });
            }
            true
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let journal = edition::Journal::load(&edition::work_dir(&app.data, &found.dir));
    Ok(EditionView { label, report, journal })
}

/// Patches the game's data from a folder or zip the player picked. The
/// report is made again first, so the plan matches the files right now.
#[tauri::command]
async fn edition_patch(
    window: tauri::Window,
    app: tauri::State<'_, App>,
    id: String,
    pack: String,
) -> Res<edition::Applied> {
    let key = format!("{id}:edition");
    {
        let mut busy = app.busy.lock().unwrap();
        if busy.contains(&key) {
            return Err("already patching".into());
        }
        busy.push(key.clone());
    }
    let _guard = BusyGuard(&app, key);
    let (game, found, manifest, mod_dirs) = edition_setup(&app, &id)?;
    let exe =
        steam::exe_in(&found.dir, &game.steam[0]).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    let exe = exe.unwrap_or_else(|| game.steam[0].exe.clone());
    let work = edition::work_dir(&app.data, &found.dir);
    let dir = found.dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let emit = |step: &str, done: usize, total: usize| {
            let _ = window.emit("edition-progress", EditionProgress { id: id.clone(), step: step.into(), done, total });
        };
        let report = edition::validate(&dir, &exe, &manifest, &mod_dirs, true, |d, t| {
            if d % 50 == 0 {
                emit("check", d, t);
            }
            true
        })?;
        edition::apply(&report, &manifest, Path::new(&pack), &work, |step, d, t| {
            if d % 20 == 0 {
                emit(step, d, t);
            }
            true
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Undoes the last patch of the Steam version, file for file.
#[tauri::command]
async fn edition_rollback(app: tauri::State<'_, App>, id: String) -> Res<edition::RolledBack> {
    let (_, found) = app.located(&id)?;
    let work = edition::work_dir(&app.data, &found.dir);
    tauri::async_runtime::spawn_blocking(move || edition::rollback(&work))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Moves XLiveLessNess's DLC packs back where they belong, after an old
/// drop-in install moved them into its own folder.
#[tauri::command]
fn xlln_restore(app: tauri::State<App>, id: String) -> Res<String> {
    let (_, found, _, mod_dirs) = edition_setup(&app, &id)?;
    let state = edition::xlln_state(&found.dir, &mod_dirs);
    let from = state.dlc_moved_to.ok_or("nothing to move back")?;
    edition::restore_xlln_dlc(&found.dir, &from).map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
fn play(app: tauri::State<App>, id: String) -> Res<()> {
    let (game, found) = app.located(&id)?;
    let rec = app.state.lock().unwrap().mods.get(&id).cloned().ok_or("install the mod first")?;
    if !cfg!(windows) {
        let root = steam::steam_roots().into_iter().next().ok_or("Steam was not found")?;
        if !launch::launch_option_ready(&root, found.appid, &app.data.join("steam-wrapper.sh")) {
            return Err("steam-setup".into());
        }
    }
    launch::start(&game, &found, &rec.dir).map_err(|e| e.to_string())
}

/// Closes Steam when `restart` is set, runs the edit, and starts Steam again.
fn with_steam_closed<T>(restart: bool, edit: impl FnOnce(&Path) -> Result<T, String>) -> Res<T> {
    let root = steam::steam_roots().into_iter().next().ok_or("Steam was not found")?;
    let was_running = steam::steam_running();
    if was_running {
        if !restart {
            return Err("steam-running".into());
        }
        if !steam::shutdown_steam(&root, std::time::Duration::from_secs(30)) {
            return Err("Steam did not close; close it and try again".into());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let out = edit(&root);
    if was_running {
        steam::start_steam(&root);
    }
    out
}

/// Linux: puts the wrapper in front of the game's launch option.
#[tauri::command]
async fn steam_setup(app: tauri::State<'_, App>, id: String, restart: bool) -> Res<()> {
    let (_, found) = app.located(&id)?;
    let wrapper = app.data.join("steam-wrapper.sh");
    tauri::async_runtime::spawn_blocking(move || {
        with_steam_closed(restart, |root| {
            launch::set_launch_option(root, found.appid, &wrapper, true).map(|_| ()).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The launcher's own artwork for a game (app/ui/brand/<id>, embedded with the page), written to
/// <data>/brand/<id> so Steam can copy it. Slots the launcher has no picture for stay empty.
fn launcher_pictures(handle: &tauri::AppHandle, data: &Path, id: &str) -> shortcuts::Pictures {
    let dir = data.join("brand").join(id);
    let slot = |name: &str| -> Option<PathBuf> {
        let asset = handle.asset_resolver().get(format!("brand/{id}/{id}-ember-{name}.png"))?;
        let path = dir.join(format!("{id}-ember-{name}.png"));
        let same = std::fs::read(&path).map(|b| b == asset.bytes).unwrap_or(false);
        if !same {
            install::write_atomic(&path, &asset.bytes).ok()?;
        }
        Some(path)
    };
    shortcuts::Pictures {
        icon: slot("icon"),
        wide: slot("wide"),
        portrait: slot("portrait"),
        hero: slot("hero"),
        logo: slot("logo"),
    }
}

/// The launcher's artwork for the game, each missing slot from the mod's own package.
fn pictures_for(
    handle: &tauri::AppHandle,
    app: &App,
    game: &egl_core::catalog::Game,
    dir: &Path,
) -> shortcuts::Pictures {
    launcher_pictures(handle, &app.data, &game.id).or(shortcuts::Pictures::from_package(game, dir))
}

#[tauri::command]
fn steam_entry_preview(handle: tauri::AppHandle, app: tauri::State<App>, id: String) -> Res<shortcuts::Entry> {
    let (game, found) = app.located(&id)?;
    let rec = app.state.lock().unwrap().mods.get(&id).cloned().ok_or("install the mod first")?;
    let pictures = pictures_for(&handle, &app, &game, &rec.dir);
    shortcuts::entry_for(&app.data, &game, &found, &rec.dir, &pictures).map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_to_steam(handle: tauri::AppHandle, app: tauri::State<'_, App>, id: String, restart: bool) -> Res<u32> {
    let (game, found) = app.located(&id)?;
    let rec = app.state.lock().unwrap().mods.get(&id).cloned().ok_or("install the mod first")?;
    let pictures = pictures_for(&handle, &app, &game, &rec.dir);
    let entry = shortcuts::entry_for(&app.data, &game, &found, &rec.dir, &pictures).map_err(|e| e.to_string())?;
    let wrapper = app.data.join("steam-wrapper.sh");
    let appid = tauri::async_runtime::spawn_blocking(move || {
        with_steam_closed(restart, |root| {
            if !cfg!(windows) {
                launch::set_launch_option(root, found.appid, &wrapper, true).map_err(|e| e.to_string())?;
            }
            shortcuts::add(root, &entry, &pictures).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut s = app.state.lock().unwrap();
    if let Some(m) = s.mods.get_mut(&id) {
        m.steam_shortcut = true;
    }
    let snapshot = s.clone();
    drop(s);
    app.save(&snapshot)?;
    Ok(appid)
}

#[tauri::command]
async fn remove_from_steam(app: tauri::State<'_, App>, id: String, restart: bool) -> Res<()> {
    let game = app.game(&id)?;
    let name = game.module.name.clone();
    tauri::async_runtime::spawn_blocking(move || {
        with_steam_closed(restart, |root| shortcuts::remove_all(root, &name).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut s = app.state.lock().unwrap();
    if let Some(m) = s.mods.get_mut(&id) {
        m.steam_shortcut = false;
    }
    let snapshot = s.clone();
    drop(s);
    app.save(&snapshot)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsView {
    outbox: Vec<logs::Bundle>,
    sent: Vec<logs::Bundle>,
    /// Log files found now, per game id.
    found: Vec<(String, usize)>,
    folder: PathBuf,
}

fn game_logs(app: &App, ids: &[String]) -> Vec<(egl_core::catalog::Game, logs::BundleGame, Vec<PathBuf>)> {
    let catalog = app.catalog.lock().unwrap().clone();
    let state = app.state.lock().unwrap().clone();
    let roots = steam::steam_roots();
    catalog
        .games
        .iter()
        .filter(|g| ids.is_empty() || ids.contains(&g.id))
        .map(|g| {
            let found = status::locate(g, &state, &roots);
            let rec = state.mods.get(&g.id);
            let dir = rec.map(|m| m.dir.clone()).or_else(|| found.as_ref().map(|f| launch::mod_dir(&app.data, g, f)));
            let files = logs::find_logs(g, dir.as_deref(), found.as_ref());
            let info = logs::BundleGame {
                id: g.id.clone(),
                mod_tag: rec.map(|m| m.tag.clone()),
                channel: rec.map(|m| m.channel.clone()),
                appid: found.as_ref().map(|f| f.appid),
            };
            (g.clone(), info, files)
        })
        .collect()
}

#[tauri::command]
fn logs_view(app: tauri::State<App>) -> LogsView {
    LogsView {
        outbox: logs::outbox(&app.data),
        sent: logs::sent(&app.data),
        found: game_logs(&app, &[]).into_iter().map(|(g, _, f)| (g.id, f.len())).collect(),
        folder: logs::logs_dir(&app.data),
    }
}

/// Zips what is new for the games (all when `ids` is empty).
#[tauri::command]
fn make_log_bundle(app: tauri::State<App>, ids: Vec<String>) -> Res<Option<logs::Bundle>> {
    let sets = game_logs(&app, &ids);
    let refs: Vec<logs::GameLogs> = sets
        .iter()
        .map(|(g, info, files)| logs::GameLogs { game: g, info: info.clone(), files: files.clone() })
        .collect();
    logs::make_bundle(&app.data, &refs).map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_log_bundle(app: tauri::State<'_, App>, file: String, note: String) -> Res<String> {
    let path = logs::logs_dir(&app.data).join("outbox").join(&file);
    if file.contains(['/', '\\']) || !path.is_file() {
        return Err("no such bundle".into());
    }
    let url = app.catalog.lock().unwrap().reports.url.clone();
    logs::upload(&app.http, url.as_deref(), &app.data, &path, &note).await.map_err(|e| e.to_string())
}

/// Opens one of the launcher's known folders in the file manager. The page
/// names it by kind, never by path.
#[tauri::command]
fn open_folder(app: tauri::State<App>, kind: String, id: Option<String>) -> Res<()> {
    let path = match (kind.as_str(), id) {
        ("data", _) => app.data.clone(),
        ("logs", _) => logs::logs_dir(&app.data),
        ("mod", Some(id)) => app.state.lock().unwrap().mods.get(&id).map(|m| m.dir.clone()).ok_or("not installed")?,
        ("game", Some(id)) => app.located(&id)?.1.dir,
        _ => return Err("unknown folder".into()),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_release_page(app: tauri::State<App>, url: String) -> Res<()> {
    // Only a GitHub release page of a repo the catalog names.
    let ok = app
        .catalog
        .lock()
        .unwrap()
        .games
        .iter()
        .flat_map(|g| g.module.channels.iter())
        .any(|c| url.starts_with(&format!("https://github.com/{}/releases/", c.repo)));
    if !ok {
        return Err("not a release page".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

/// The newest catalog: the launcher repo's when it answers and parses, else
/// the last one fetched, else the copy built in. EMBER_GAMES_CATALOG names a
/// catalog file to use instead, to try a catalog change before publishing it.
async fn load_catalog(http: &reqwest::Client, data: &Path) -> (Catalog, String) {
    if let Some(file) = std::env::var_os("EMBER_GAMES_CATALOG") {
        match std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| Catalog::parse(&t).map_err(|e| e.to_string()))
        {
            Ok(c) => return (c, "file".into()),
            Err(e) => eprintln!("EMBER_GAMES_CATALOG {}: {e}; using the usual catalog", Path::new(&file).display()),
        }
    }
    let cache = data.join("catalog.json");
    if let Ok(text) = install::fetch_text(http, CATALOG_URL).await {
        if let Ok(c) = Catalog::parse(&text) {
            let _ = install::write_atomic(&cache, text.as_bytes());
            return (c, "online".into());
        }
    }
    if let Some(c) = std::fs::read_to_string(&cache).ok().and_then(|t| Catalog::parse(&t).ok()) {
        return (c, "saved".into());
    }
    (Catalog::bundled(), "built in".into())
}

fn main() {
    let data = install::data_dir();
    let _ = std::fs::create_dir_all(&data);
    let state = State::load(&data.join("state.json"));
    let http = install::client();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(App {
            data: data.clone(),
            catalog: Mutex::new(Catalog::bundled()),
            catalog_source: Mutex::new("built in".into()),
            state: Mutex::new(state),
            http: http.clone(),
            busy: Mutex::new(Vec::new()),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let (c, source) = load_catalog(&http, &data).await;
                let st = handle.state::<App>();
                *st.catalog.lock().unwrap() = c;
                *st.catalog_source.lock().unwrap() = source;
                let _ = handle.emit("catalog-updated", ());
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            overview,
            latest,
            set_channel,
            set_folder,
            install_mod,
            adopt_mod,
            check_mod,
            uninstall_mod,
            edition_check,
            edition_patch,
            edition_rollback,
            xlln_restore,
            play,
            steam_setup,
            steam_entry_preview,
            add_to_steam,
            remove_from_steam,
            logs_view,
            make_log_bundle,
            send_log_bundle,
            open_folder,
            open_release_page
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Ember Games Launcher");
}
