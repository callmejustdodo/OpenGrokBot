#!/bin/sh
# Builds a signed release of the app and, with --publish, publishes it as a
# GitHub release the installed apps update from (tech spec, "Distribution").
#
# The version comes from apps/desktop/src-tauri/tauri.conf.json; raise it
# (with the workspace version in Cargo.toml) before each release, since the
# updater installs only a higher version. The release must be a full release,
# not a pre-release or draft: the updater reads releases/latest, which skips
# those.
#
# Usage: pnpm -C apps/desktop release [--publish]
# Needs: the updater private key at $TAURI_SIGNING_PRIVATE_KEY_PATH
# (default ~/.tauri/opengrokbot-updater.key), and the GitHub CLI for --publish.
set -eu
cd "$(dirname "$0")/../../.."

publish=false
case "${1:-}" in
  --publish) publish=true ;;
  "") ;;
  *) echo "usage: $0 [--publish]" >&2; exit 2 ;;
esac

key="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/opengrokbot-updater.key}"
if [ ! -f "$key" ]; then
  echo "updater signing key not found at $key" >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "the working tree has changes; release from a clean checkout" >&2
  exit 1
fi

if [ "$(uname -m)" != arm64 ]; then
  echo "release builds are made on an Apple Silicon Mac (the updater manifest lists darwin-aarch64 only)" >&2
  exit 1
fi

version=$(node -p 'require("./apps/desktop/src-tauri/tauri.conf.json").version')
commit=$(git rev-parse HEAD)
tag="v$version"
arch=aarch64
repo=callmejustdodo/OpenGrokBot

if [ "$publish" = true ]; then
  git fetch -q origin
  if [ -z "$(git branch -r --contains "$commit")" ]; then
    echo "$commit is not on GitHub; push it before publishing" >&2
    exit 1
  fi
  if gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
    echo "release $tag already exists; raise the version first" >&2
    exit 1
  fi
fi

TAURI_SIGNING_PRIVATE_KEY=$(cat "$key")
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
pnpm -C apps/desktop tauri build --config '{"bundle":{"createUpdaterArtifacts":true}}'

bundle=target/release/bundle
out=target/release/github-release
rm -rf "$out"
mkdir -p "$out"
archive="OpenGrokBot_${version}_${arch}.app.tar.gz"
cp "$bundle/macos/OpenGrokBot.app.tar.gz" "$out/$archive"
cp "$bundle/macos/OpenGrokBot.app.tar.gz.sig" "$out/$archive.sig"
cp "$bundle/dmg/OpenGrokBot_${version}_${arch}.dmg" "$out/"

VERSION="$version" URL="https://github.com/$repo/releases/download/$tag/$archive" \
  SIG_FILE="$out/$archive.sig" node -e '
const fs = require("fs");
const manifest = {
  version: process.env.VERSION,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": {
      signature: fs.readFileSync(process.env.SIG_FILE, "utf8").trim(),
      url: process.env.URL,
    },
  },
};
process.stdout.write(JSON.stringify(manifest, null, 2) + "\n");
' > "$out/latest.json"

echo "release files for $tag ($commit) in $out:"
ls -1 "$out"

if [ "$publish" = true ]; then
  gh release create "$tag" --repo "$repo" --target "$commit" --title "OpenGrokBot $version" \
    --generate-notes --latest \
    "$out/OpenGrokBot_${version}_${arch}.dmg" "$out/$archive" "$out/$archive.sig" "$out/latest.json"
fi
