#!/bin/sh
# Fails if static/app.css isn't what scripts/css.sh builds from the
# templates and crates as they are: the stylesheet is committed (so
# `cargo build` needs no extra step), and a class added to a template, or
# one no template uses any more, leaves it stale. CI's css job runs this;
# run it before pushing, after any change to templates/, crates/ or
# assets/app.css.
set -eu
cd "$(dirname "$0")/.."
scripts/css.sh >/dev/null 2>&1
if ! git diff --quiet static/app.css; then
    git --no-pager diff --stat static/app.css
    git --no-pager diff static/app.css | head -200
    echo "static/app.css is stale: scripts/css.sh rebuilt it; commit the result." >&2
    exit 1
fi
