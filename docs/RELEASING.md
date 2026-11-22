# Releasing the launcher

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull request:

- Linux: `cargo fmt --check`, clippy with warnings as errors, every test in the workspace (launcher core, app,
  ember-logs), and a parse check of the page's script.
- Windows: clippy and the core tests, so the Windows-only code (registry, Steam paths) builds and passes there.

## A release

1. `tools/bump-version.sh 0.2.0` sets the version in the workspace `Cargo.toml` (the only place it is written;
   the app, the installers and the Settings page read it from there) and commits it.
2. Push, then tag: `git push && git tag -a v0.2.0 -m "Ember Games Launcher 0.2.0" && git push origin v0.2.0`.
3. `.github/workflows/release.yml` (a manual run only checks that a branch builds; it keeps no files) checks the tag against the version, runs the core tests on both systems and
   builds:
   - `ember-games-launcher-<version>-windows-x64-setup.exe` (NSIS installer)
   - `ember-games-launcher-<version>-linux-x86_64.AppImage` (built on Ubuntu 22.04, so it runs on older glibc and
     on SteamOS)
   - `ember-games-launcher-<version>-linux-amd64.deb`
   - `.sig` files next to the installer and the AppImage (the updater signatures)
   - `latest.json` (what installed launchers read, see below)
   - `SHA256SUMS`
4. It makes a **draft** release first, and each build job adds its files to it directly (never Actions
   artifacts: that storage is shared across the account and was full). Notes come from the commits. Nothing is
   public until the draft is edited and published by hand. A tag with a suffix (`v0.2.0-beta.1`) is marked as a pre-release.

## The launcher's own update

At every start the launcher reads `latest.json` from the latest **published** release
(`releases/latest/download/latest.json`), so drafts and pre-releases are never offered. When it lists a newer
version, a bar at the top offers "Update and restart". The download must carry a signature from the updater key
whose public half is in `app/src-tauri/tauri.conf.json`; anything else is refused and nothing is installed. The
update is refused while a game of the catalog (or its mod launcher) is running or a mod install is under way.

- Windows: the downloaded installer runs in passive mode (a progress window, no questions) and starts the new
  launcher.
- Linux AppImage: the AppImage file is replaced and restarted.
- Linux .deb: not updated in place (the package manager owns it); the bar links to the release page instead.

The key: secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the GitHub environment
`updater-signing`, which only `v*.*.*` tags may use, and a backup in the maintainer's password store
(`ember-games-launcher/updater-signing-key` and `-password`). **If the key is lost, installed launchers can never
take an update again**; they would need the new installer by hand. The builds run without the key
(`--config tauri.unsigned.conf.json`, so no third-party build script can read it); the `sign` job then signs the
installer and the AppImage in a step that runs only the pinned Tauri signer. A local build without the key needs the
same `--config tauri.unsigned.conf.json`.

To test an update without publishing: build two signed installers (say 0.3.1 and 0.3.99) with
`"dangerousInsecureTransportProtocol": true` added to the updater config **in a scratch copy only**, serve a
`latest.json` for the newer one on `http://127.0.0.1`, install the older one and start it with
`EMBER_GAMES_UPDATE_URL=http://127.0.0.1:<port>/latest.json`. The signature is still checked against the built-in
key.

## Pinned tools

- Rust: `rust-toolchain.toml` (1.95.0, with rustfmt and clippy). Local builds use it too; bump it on purpose.
- Tauri CLI: `app/src-tauri/package.json` and `package-lock.json` (2.12.4, with npm integrity hashes). The release
  jobs run `npm ci` and `npx --no-install tauri build`, so a changed package on npm fails the build instead of
  ending up in an installer. Bump it with `npm install -D --save-exact @tauri-apps/cli@<version>` in that folder.

## Not yet

- The Windows installer is not code-signed, so SmartScreen warns on first run.
- Launchers older than the first version with the updater (0.3.1 and before) do not update themselves; their
  players need the new installer once.
