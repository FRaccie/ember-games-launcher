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
3. `.github/workflows/release.yml` checks the tag against the version, runs the core tests on both systems and
   builds:
   - `ember-games-launcher-<version>-windows-x64-setup.exe` (NSIS installer)
   - `ember-games-launcher-<version>-linux-x86_64.AppImage` (built on Ubuntu 22.04, so it runs on older glibc and
     on SteamOS)
   - `ember-games-launcher-<version>-linux-amd64.deb`
   - `SHA256SUMS`
4. It makes a **draft** release with those files and notes generated from the commits. Nothing is public until
   the draft is edited and published by hand. A tag with a suffix (`v0.2.0-beta.1`) is marked as a pre-release.

## Pinned tools

- Rust: `rust-toolchain.toml` (1.95.0, with rustfmt and clippy). Local builds use it too; bump it on purpose.
- Tauri CLI: `app/src-tauri/package.json` and `package-lock.json` (2.12.4, with npm integrity hashes). The release
  jobs run `npm ci` and `npx --no-install tauri build`, so a changed package on npm fails the build instead of
  ending up in an installer. Bump it with `npm install -D --save-exact @tauri-apps/cli@<version>` in that folder.

## Not yet

- The Windows installer is not code-signed, so SmartScreen warns on first run.
- The launcher does not update itself yet; a new version is installed by running the new installer.
