#!/usr/bin/env bash
# Refreshes Tether's built-in static data (crates/sde/data) from CCP's
# static data export: the newest build at developers.eveonline.com, or the
# build number given. Run it from a developer's shell when CCP publishes
# a new SDE that apps need (new items, new ores); commit the result. The
# image build and the running app never fetch it (CLAUDE.md, Opsec).
#
#   scripts/update-sde.sh            # the newest build
#   scripts/update-sde.sh 3579973    # a given build
set -euo pipefail

base="https://developers.eveonline.com/static-data/tranquility"
build="${1:-}"
if [ -z "$build" ]; then
  build="$(curl -fsSL "$base/latest.jsonl" | sed -n 's/.*"buildNumber": *\([0-9]*\).*/\1/p' | head -n 1)"
fi
case "$build" in
  ''|*[!0-9]*) echo "No SDE build number found." >&2; exit 1 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Downloading SDE build $build"
curl -fsSL -o "$work/sde.zip" "$base/eve-online-static-data-$build-jsonl.zip"
unzip -q "$work/sde.zip" -d "$work/sde"
cargo run -q -p tether-sde --bin sde-extract -- "$work/sde" crates/sde/data
echo "crates/sde/data now holds SDE build $build: review and commit it."
