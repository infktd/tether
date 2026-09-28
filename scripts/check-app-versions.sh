#!/usr/bin/env bash
# Every first-party app (plugins/*) whose files changed since BASE must
# carry a newer version than it had there (a patch bump, 0.0.1, at least),
# in plugin.toml and Cargo.toml alike. Installs tell versions apart by
# number: a changed app under an old number is a conflict on every
# instance's Apps page.
#
#   scripts/check-app-versions.sh origin/main   # before pushing
#   scripts/check-app-versions.sh <commit>      # CI: what the push started from
set -euo pipefail

base="${1:?usage: $0 <base commit>}"
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "No commit $base to compare with; nothing to check."
  exit 0
fi

version_in() { sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1; }

failed=0
for dir in plugins/*/; do
  dir="${dir%/}"
  [ -f "$dir/plugin.toml" ] || continue
  app="${dir#plugins/}"
  now="$(version_in < "$dir/plugin.toml")"
  crate="$(version_in < "$dir/Cargo.toml")"
  if [ "$now" != "$crate" ]; then
    echo "::error::$app: plugin.toml says $now but Cargo.toml says $crate; keep them the same."
    failed=1
  fi
  # New since BASE: nothing to be newer than.
  git cat-file -e "$base:$dir/plugin.toml" 2>/dev/null || continue
  if git diff --quiet "$base" -- "$dir"; then
    continue
  fi
  was="$(git show "$base:$dir/plugin.toml" | version_in)"
  newest="$(printf '%s\n%s\n' "$was" "$now" | sort -V | tail -n 1)"
  if [ "$now" = "$was" ] || [ "$newest" != "$now" ]; then
    echo "::error::$app changed since $base but its version is $now (was $was): bump it, e.g. by 0.0.1, in plugin.toml and Cargo.toml."
    failed=1
  else
    echo "$app: $was -> $now"
  fi
done
exit "$failed"
