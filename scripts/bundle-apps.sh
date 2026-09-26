#!/usr/bin/env bash
# Builds the first-party apps (every crate in plugins/) and packages each
# as an unsigned Tether package, the way the app image bundles them:
#   scripts/bundle-apps.sh dist/apps
#   BUNDLED_APPS_DIR=dist/apps cargo run -p tether-server --features dev
# deploy/Dockerfile runs it into /usr/share/tether/apps. Needs zip.
#
# A bundled package has no [publisher] key and no signature: it ships in
# the same image as Tether and is exactly as trusted (see
# crates/web/src/bundled.rs). Packages for anywhere else are signed with
# scripts/package-plugin.sh.
set -euo pipefail

out=${1:?usage: bundle-apps.sh <output dir>}
command -v zip >/dev/null || { echo "zip isn't installed" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$(dirname "$0")/.."

field() { sed -n "s/^$1 = \"\(.*\)\"/\1/p" "$2" | head -1; }

crates=()
for dir in plugins/*/; do
    crates+=(-p "$(field name "$dir/Cargo.toml")")
done
cargo build --locked --release --target wasm32-wasip2 "${crates[@]}"
target=${CARGO_TARGET_DIR:-target}

rm -f "$out"/*.zip
for dir in plugins/*/; do
    dir=${dir%/}
    crate=$(field name "$dir/Cargo.toml")
    id=$(field id "$dir/plugin.toml")
    version=$(field version "$dir/plugin.toml")
    work=$(mktemp -d)
    # Without the [publisher] table (the signing placeholder).
    awk '/^\[publisher\]$/ { skip = 1; next } /^\[/ { skip = 0 } !skip' \
        "$dir/plugin.toml" > "$work/plugin.toml"
    if grep -q 'PUBLISHER_KEY\|^\[publisher\]' "$work/plugin.toml"; then
        echo "$dir/plugin.toml: the [publisher] table wasn't removed" >&2
        exit 1
    fi
    cp "$target/wasm32-wasip2/release/${crate//-/_}.wasm" "$work/plugin.wasm"
    [[ -d $dir/migrations ]] && cp -R "$dir/migrations" "$work/migrations"
    package="$out/$id-$version.zip"
    (cd "$work" && zip -X -r -q "$package" plugin.toml plugin.wasm $( [[ -d migrations ]] && echo migrations ))
    rm -rf "$work"
    echo "$package"
done
