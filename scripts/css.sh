#!/bin/sh
# Builds static/app.css with Tailwind's standalone CLI (no Node, no npm).
# Downloads the pinned CLI into tools/ on first use and verifies its
# checksum. static/app.css is committed; CI rebuilds it and fails on drift.
#
#   scripts/css.sh           build
#   scripts/css.sh --watch   rebuild on change
set -eu

VERSION=v4.3.3
cd "$(dirname "$0")/.."

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  asset=tailwindcss-macos-arm64 sha=cdf646702987a743464dff4d9c60fd4480d1c1e73dd819a9a67f1078815dce9d ;;
    Linux-x86_64)  asset=tailwindcss-linux-x64   sha=dc61b3ac6b8c9ca874c0cc4c57b2409791a64c5540404ca5f5367360babc313a ;;
    Linux-aarch64) asset=tailwindcss-linux-arm64 sha=55fd0b241214eff3de1e8ee4f22796662f2d2e7a49bcfca7477cfd0bac398195 ;;
    *) echo "No pinned Tailwind CLI for $(uname -s)-$(uname -m); add one to scripts/css.sh." >&2; exit 1 ;;
esac

bin=tools/bin/tailwindcss-$VERSION
if [ ! -x "$bin" ]; then
    mkdir -p tools/bin
    curl -fsSL -o "$bin.tmp" "https://github.com/tailwindlabs/tailwindcss/releases/download/$VERSION/$asset"
    actual=$( (sha256sum "$bin.tmp" 2>/dev/null || shasum -a 256 "$bin.tmp") | cut -d' ' -f1)
    if [ "$actual" != "$sha" ]; then
        rm -f "$bin.tmp"
        echo "Checksum mismatch for $asset: got $actual" >&2
        exit 1
    fi
    chmod +x "$bin.tmp"
    mv "$bin.tmp" "$bin"
fi

exec "$bin" -i assets/app.css -o static/app.css --minify "$@"
