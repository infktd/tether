#!/usr/bin/env bash
# Builds a first-party plugin into a signed package Tether installs:
#   scripts/package-plugin.sh plugins/moon-mining ~/.minisign/tether.key
# needs minisign and zip. The public key (the .pub next to the secret key)
# goes into the package's plugin.toml; keep the secret key out of the repo.
set -euo pipefail

dir=${1:?usage: package-plugin.sh <plugin dir> <minisign secret key>}
key=${2:?usage: package-plugin.sh <plugin dir> <minisign secret key>}
pub=${key%.key}.pub
[[ -f $pub ]] || { echo "no public key at $pub" >&2; exit 1; }
command -v minisign >/dev/null || { echo "minisign isn't installed" >&2; exit 1; }
command -v zip >/dev/null || { echo "zip isn't installed" >&2; exit 1; }

crate=$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)
id=$(sed -n 's/^id = "\(.*\)"/\1/p' "$dir/plugin.toml" | head -1)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$dir/plugin.toml" | head -1)
public_key=$(sed -n 2p "$pub")

cargo build -p "$crate" --target wasm32-wasip2 --release
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
sed "s|^key = \"PUBLISHER_KEY\"|key = \"$public_key\"|" "$dir/plugin.toml" > "$out/plugin.toml"
if grep -q '^key = "PUBLISHER_KEY"' "$out/plugin.toml"; then
    echo "the publisher key wasn't filled in" >&2
    exit 1
fi
cp "target/wasm32-wasip2/release/${crate//-/_}.wasm" "$out/plugin.wasm"
[[ -d $dir/migrations ]] && cp -R "$dir/migrations" "$out/migrations"
package="$PWD/dist/$id-$version.zip"
mkdir -p dist
rm -f "$package" "$package.minisig"
(cd "$out" && zip -X -r -q "$package" plugin.toml plugin.wasm $( [[ -d migrations ]] && echo migrations ))
minisign -S -s "$key" -m "$package"
# The .pub must be the key that signed, or Tether refuses the package.
minisign -V -q -p "$pub" -m "$package"
echo "$package"
echo "$package.minisig"
