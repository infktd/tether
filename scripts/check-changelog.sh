#!/usr/bin/env bash
# A push that changes what Tether does (code, templates, assets, apps,
# migrations, the image) adds a CHANGELOG.md entry: pilots see it in the
# "What's new" popup after the update (crates/web-core/src/whats_new.rs).
# Tests, docs and CI alone need none.
#
#   scripts/check-changelog.sh origin/main   # before pushing
#   scripts/check-changelog.sh <commit>      # CI: what the push started from
set -euo pipefail

base="${1:?usage: $0 <base commit>}"
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "No commit $base to compare with; nothing to check."
  exit 0
fi

entries() { grep -c '^## ' || true; }

changed="$(git diff --name-only "$base" -- crates plugins templates assets static migrations wit deploy \
  | grep -v -E '(^|/)tests?/|\.md$' || true)"
if [ -z "$changed" ]; then
  echo "Nothing pilots would notice changed since $base."
  exit 0
fi
was=0
if git cat-file -e "$base:CHANGELOG.md" 2>/dev/null; then
  was="$(git show "$base:CHANGELOG.md" | entries)"
fi
now="$(entries < CHANGELOG.md)"
if [ "$now" -le "$was" ]; then
  echo "::error::What Tether does changed since $base, but CHANGELOG.md has no new entry. Add one at the top (## YYYY-MM-DD, then ### Everyone, ### Admins or ### <app name> notes) saying it in plain words. Changed:"
  echo "$changed" | head -n 20
  exit 1
fi
echo "CHANGELOG.md: $was -> $now entries."
