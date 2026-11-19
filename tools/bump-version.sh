#!/bin/sh
# Sets the launcher's version (the workspace Cargo.toml is the only place it is written; the app, its installer
# and the Settings page read it from there) and commits it. Tagging is a separate, deliberate step:
#   tools/bump-version.sh 0.2.0
#   git push && git tag -a v0.2.0 -m "Ember Games Launcher 0.2.0" && git push origin v0.2.0
set -eu
v="${1:?usage: tools/bump-version.sh <version, e.g. 0.2.0 or 0.2.0-beta.1>}"
echo "$v" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || { echo "not a version: $v" >&2; exit 1; }
cd "$(dirname "$0")/.."
git diff --quiet && git diff --cached --quiet || { echo "commit or stash your changes first" >&2; exit 1; }
sed -i.bak "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"$v\"/" Cargo.toml && rm -f Cargo.toml.bak
grep -q "^version = \"$v\"" Cargo.toml || { echo "could not set the version in Cargo.toml" >&2; exit 1; }
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
git commit -q -am "Version $v"
echo "Committed version $v. To release: git push && git tag -a v$v -m \"Ember Games Launcher $v\" && git push origin v$v"
