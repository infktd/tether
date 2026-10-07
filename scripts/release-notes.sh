#!/usr/bin/env bash
# Prints a release's notes in Markdown, from CHANGELOG.md: the entries added
# since the previous release. CI's publish-release job makes them the
# GitHub release's notes (deploy/README.md, Releasing).
#
# Entries are never removed or reordered (crates/web-core/src/whats_new.rs),
# so the release's new entries are the top ones: as many as CHANGELOG.md
# gained between the previous release tag and this one. The previous release
# is the nearest vX.Y.Z tag in this tag's history; with none, every entry is
# new. The notes are grouped by heading (Everyone, then each app, then
# Admins), newest first in each group.
#
#   scripts/release-notes.sh v1.2.0           # since the release before it
#   scripts/release-notes.sh v1.2.0 v1.1.0    # since a given tag or commit
set -euo pipefail

die() { echo "$*" >&2; exit 1; }

tag="${1:?usage: $0 <release tag> [<previous tag or commit>]}"
git rev-parse --verify --quiet "$tag^{commit}" >/dev/null || die "No tag or commit $tag."

prev="${2:-}"
if [ -z "$prev" ]; then
  # The nearest release tag this one comes after, by commits between them.
  best=""
  for t in $(git tag --merged "$tag" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true); do
    [ "$t" = "$tag" ] && continue
    distance="$(git rev-list --count "$t..$tag")"
    if [ -z "$best" ] || [ "$distance" -lt "$best" ]; then
      best="$distance"
      prev="$t"
    fi
  done
fi
if [ -n "$prev" ]; then
  git rev-parse --verify --quiet "$prev^{commit}" >/dev/null || die "No tag or commit $prev."
fi

entries() { grep -c '^## ' || true; }

changelog="$(git show "$tag:CHANGELOG.md" 2>/dev/null)" || die "$tag has no CHANGELOG.md."
now="$(printf '%s\n' "$changelog" | entries)"
was=0
if [ -n "$prev" ] && git cat-file -e "$prev:CHANGELOG.md" 2>/dev/null; then
  was="$(git show "$prev:CHANGELOG.md" | entries)"
fi
new=$((now - was))

if [ -n "$prev" ]; then
  echo "What changed since $prev."
else
  echo "What changed in Tether up to this release."
fi
echo

# The top $new entries, their notes collected under each heading. A note is
# a "- " line and the indented lines after it, as whats_new.rs reads them.
if [ "$new" -le 0 ]; then
  printf 'Nothing pilots would notice changed.\n\n'
else
  printf '%s\n' "$changelog" | awk -v new="$new" '
    /^## / { entry++; group = ""; inside = 0; next }
    entry == 0 || entry > new { next }
    /^### / {
      group = substr($0, 5); inside = 0
      if (!(group in seen)) { seen[group] = 1; order[++groups] = group }
      next
    }
    group == "" { next }
    /^- / { notes[group] = notes[group] $0 "\n"; inside = 1; next }
    /^  / && $0 !~ /^ *$/ && inside { notes[group] = notes[group] $0 "\n"; next }
    function show(g) {
      if (notes[g] == "") return
      printf "### %s\n%s\n", g, notes[g]
    }
    END {
      show("Everyone")
      for (i = 1; i <= groups; i++)
        if (order[i] != "Everyone" && order[i] != "Admins") show(order[i])
      show("Admins")
    }
  '
fi

version="${tag#v}"
if [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Upgrade from Administration › Health, or on the server with \`deploy/install.sh --version $version\` (deploy/README.md)."
fi
