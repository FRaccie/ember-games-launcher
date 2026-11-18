# Ember Games Launcher

One app for the Ember netplay mods on Windows and Linux (Steam Deck included):

- finds the games in every Steam library (USF4, SFxT, UMvC3, SFV), or in a folder you pick;
- downloads each mod from its GitHub releases, checks it against GitHub's sha256 and the published checksum file, and unpacks it into a folder of its own, never into the game's folder;
- updates in place and keeps the files the mod wrote there (settings, logs);
- adds a mod to Steam as its own entry, with its artwork, so it starts from Big Picture and Game Mode;
- starts the mod through the mod's own launcher;
- zips the mods' logs per game or for all games, with only what is new since the last bundle that was sent.

It sits on top of each mod: the mods keep their own launchers and updaters.

## Catalog

`catalog/catalog.json` lists the games, their Steam apps (the version most players use first, so SFxT checks
55550 GFWL before 209120), and per mod: its launcher, the option that names the game folder, its release
channels (repo, tag and asset patterns) and its log folders. The app reads the copy on `main` of this repo,
then the last copy it fetched, then the one built in. While the repo is private the built-in copy is used.

Adding a mod or a channel is a catalog change, not a code change.

## How starting works

- **Windows:** the mod's launcher runs from the mod's folder, with `--game <folder>` when the mod takes it.
- **Linux:** the mod goes inside the game's Proton prefix (`drive_c/EmberGames/<mod>`), as Ember's Steam Deck
  installers do. `steam-wrapper.sh` goes in front of the game's Steam launch option; when the game is started
  with `-ember` it swaps the game's exe for the mod's launcher inside the game's own Proton session. The game's
  own Play button still starts the plain game. The launcher (and the Steam entry) start
  `steam -applaunch <appid> -ember`.

Steam rewrites `shortcuts.vdf` and `localconfig.vdf` when it exits, so both are edited only while Steam is
closed. The app asks, closes Steam, edits (keeping a `.bak`), and starts Steam again.

## Logs

Log folders come from the catalog (`{appdata}` is the player's Roaming folder, inside the Proton prefix on
Linux). A bundle is a zip with `manifest.json` and one folder per game. `logs/sent.json` remembers how far each
file was sent; a log that only grew contributes just its new part, a replaced log goes in whole. A log never
sent goes in only when it changed in the last 7 days, newest first, at most 30 files and 32 MB per game. The
account name is taken out of paths. Bundles wait in `logs/outbox` and move to `logs/sent` after an upload.

**Upload target: not decided.** `reports.url` in the catalog is empty, so bundles are only saved. The intended
target is Ember's report intake (`ember-reports`, which forwards to Bugsink), once it accepts a `logs` kind with
a zip part; its schema 1 takes only SF4's four log names and no zip today.

## Layout

```
catalog/catalog.json         games, mods, channels, log folders
crates/egl-core              all the logic, tested without a window
  steam.rs                   Steam folders, libraries, manifests, users, Proton prefix
  vdf.rs                     text and binary VDF
  releases.rs                picking a channel's release, checksums
  install.rs                 download, verify, unpack, swap in, state
  launch.rs                  starting, Linux wrapper and launch options
  shortcuts.rs               Steam entries and artwork
  logs.rs                    log bundles and the sent ledger
  examples/egl-check.rs      read-only checks against GitHub and this PC
app/src-tauri                Tauri 2 shell (commands only)
app/ui                       the page (plain HTML, CSS, JS)
```

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p egl-core --example egl-check -- releases   # newest release per channel, live
cargo run -p egl-core --example egl-check -- scan       # games and logs on this PC, read-only
cargo build --release -p ember-games-launcher           # the app
```

Linux needs `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev`. Installers
(`nsis`, `appimage`, `deb`) come from `cargo tauri build` in `app/src-tauri`. `tools/make-icons.py` redraws the
icons.

## What is checked and what is not

Checked: unit tests (VDF, detection, catalog patterns, release picking, checksums, unpacking with unsafe paths,
updates that keep player files, Steam entries, launch options, the wrapper script, log bundles); every catalog
channel against the live GitHub releases; all four real packages unpacked; detection and a real log bundle on a
Windows PC with all four games; the page rendered in a browser with a stand-in for the app.

Not yet run for real: installing and starting a mod from the app window, Add to Steam against a real Steam,
anything on Linux or a Steam Deck, controller navigation with a pad.
