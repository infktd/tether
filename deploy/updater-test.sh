#!/bin/sh
# Tests for updater.sh, with a fake docker that records what it's asked.
# Runs in the updater's own image, as CI does:
#
#   docker run --rm -v "$PWD/deploy:/src:ro" docker:28.5.2-cli sh /src/updater-test.sh
#
# Each check's condition is quoted, for check() to evaluate.
# shellcheck disable=SC2016

set -eu

src=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

A=$(printf 'a%.0s' $(seq 64))
B=$(printf 'b%.0s' $(seq 64))

# The fake docker: the running app is sha256:aaa..., 1.2.0 is sha256:bbb...
mkdir -p "$work/bin"
cat > "$work/bin/docker" <<'FAKE'
#!/bin/sh
echo "docker $*" >> "$LOG"
case "$*" in
    "compose ps -q app") echo cid1 ;;
    "inspect -f {{.Image}} cid1") echo "${RUNNING_IMAGE:-img-running}" ;;
    "inspect -f {{.State.Running}} {{.State.Restarting}} cid1") echo "${RUNNING:-true false}" ;;
    "image inspect -f"*" img-running") echo "ghcr.io/acme/tether@sha256:$A" ;;
    "image inspect -f"*" img-new") echo "ghcr.io/acme/tether@sha256:$B" ;;
    "image inspect -f"*" ghcr.io/acme/tether:"*) echo "ghcr.io/acme/tether@sha256:${PULLED:-$B}" ;;
    "compose pull app") exit "${FAIL_PULL:-0}" ;;
    "compose run --rm --no-deps app rollback"*) exit "${FAIL_RESTORE:-0}" ;;
esac
exit 0
FAKE
chmod +x "$work/bin/docker"

# setup IMAGE: a fresh deploy directory and volumes.
setup() {
    rm -rf "$work/deploy" "$work/requests" "$work/status"
    mkdir -p "$work/deploy" "$work/requests" "$work/status"
    printf 'DOMAIN=example.com\nTETHER_IMAGE=%s\nPOSTGRES_PASSWORD=secret\n' "$1" > "$work/deploy/.env"
    chmod 0600 "$work/deploy/.env"
    : > "$work/log"
}

# run REQUEST [ENV...]: one pass of the updater over a request.
run() {
    printf '%s' "$1" > "$work/requests/request"
    shift
    env PATH="$work/bin:$PATH" LOG="$work/log" A="$A" B="$B" \
        TETHER_DEPLOY_DIR="$work/deploy" REQUESTS_DIR="$work/requests" STATUS_DIR="$work/status" \
        TETHER_UID="$(id -u)" SETTLE_SECONDS=0 STEP_SECONDS=0 UPDATER_ONCE=1 "$@" \
        sh "$src/updater.sh" > "$work/out" 2>&1 || true
}

check() {
    if eval "$2"; then
        echo "ok   $1"
    else
        echo "FAIL $1"
        echo "  status: $(cat "$work/status/status.json" 2>/dev/null)"
        echo "  .env:   $(grep TETHER_IMAGE "$work/deploy/.env")"
        sed 's/^/  /' "$work/log"
        failures=$((failures + 1))
    fi
}

state() {
    sed -n 's/.*"state":"\([a-z]*\)".*/\1/p' "$work/status/status.json"
}

image() {
    sed -n 's/^TETHER_IMAGE=//p' "$work/deploy/.env"
}

setup ghcr.io/acme/tether:edge
run 'id=0123456789abcdef
action=upgrade
tag=1.2.0'
check "upgrades to a published tag" '[ "$(state)" = done ] && [ "$(image)" = ghcr.io/acme/tether:1.2.0 ]'
check "pulls, then recreates only the app" 'grep -qx "docker compose pull app" "$work/log" && grep -qx "docker compose up -d --no-deps app" "$work/log"'
check "remembers the image it replaced, and the one it started" 'grep -qx "from=ghcr.io/acme/tether@sha256:$A" "$work/status/previous" && grep -qx "to=ghcr.io/acme/tether@sha256:$B" "$work/status/previous"'
check "keeps .env private" '[ "$(stat -c %a "$work/deploy/.env")" = 600 ]'

# The app now runs 1.2.0 (sha256:bbb...).
export RUNNING_IMAGE=img-new
check "keeps the rest of .env and its mode" 'grep -qx POSTGRES_PASSWORD=secret "$work/deploy/.env" && [ "$(stat -c %a "$work/deploy/.env")" = 600 ]'
check "takes the request away" '[ ! -e "$work/requests/request" ]'
check "writes a heartbeat" '[ -s "$work/status/alive" ]'

run 'id=0123456789abcdef
action=rollback
snapshot=core-20260928T101500Z.tsnap'
check "rolls back to the recorded image" '[ "$(state)" = done ] && [ "$(image)" = "ghcr.io/acme/tether@sha256:$A" ]'
check "stops, restores the snapshot, starts" 'grep -qx "docker compose stop app" "$work/log" && grep -qx "docker compose run --rm --no-deps app rollback --yes --snapshot core-20260928T101500Z.tsnap" "$work/log"'
check "one step only" '[ ! -e "$work/status/previous" ]'
unset RUNNING_IMAGE

: > "$work/log"
run 'id=0123456789abcdef
action=rollback'
check "nothing to roll back to" '[ "$(state)" = failed ] && ! grep -q "compose stop" "$work/log"'

setup ghcr.io/acme/tether:edge
for bad in '1.2.0;reboot' '../1.2.0' 'v1.2.0' '1.2.0-rc1' 'evil.example/x:1'; do
    run "id=0123456789abcdef
action=upgrade
tag=$bad"
    check "refuses the tag '$bad'" '[ "$(state)" = failed ] && [ "$(image)" = ghcr.io/acme/tether:edge ] && [ ! -s "$work/log" ]'
done

run 'id=0123456789abcdef
action=upgrade
tag=edge
image=evil.example/tether'
check "never takes an image from the request" '[ "$(image)" = ghcr.io/acme/tether:edge ] && ! grep -q evil "$work/log"'

setup ghcr.io/acme/tether:edge
run 'id=nothex!
action=upgrade
tag=1.2.0'
check "refuses an unreadable request" '[ "$(state)" = failed ] && [ ! -s "$work/log" ]'

setup ghcr.io/acme/tether:edge
run 'id=0123456789abcdef
action=rollback
snapshot=../../etc/passwd'
printf 'from=ghcr.io/acme/tether@sha256:%s\nto=ghcr.io/acme/tether@sha256:%s\n' "$B" "$A" > "$work/status/previous"
run 'id=0123456789abcdef
action=rollback
snapshot=../../etc/passwd'
check "refuses a snapshot path" '[ "$(state)" = failed ] && ! grep -q "compose stop" "$work/log"'

setup ghcr.io/acme/tether:edge
printf 'from=evil.example/tether@sha256:%s\nto=ghcr.io/acme/tether@sha256:%s\n' "$B" "$A" > "$work/status/previous"
run 'id=0123456789abcdef
action=rollback'
check "rolls back only within this install's repository" '[ "$(state)" = failed ] && [ "$(image)" = ghcr.io/acme/tether:edge ]'

setup ghcr.io/acme/tether:edge
printf 'from=ghcr.io/acme/tether@sha256:%s\nto=ghcr.io/acme/tether@sha256:%s\n' "$B" "$B" > "$work/status/previous"
run 'id=0123456789abcdef
action=rollback'
check "no rollback once Tether moved outside the console" '[ "$(state)" = failed ] && grep -q "outside the console" "$work/status/status.json" && ! grep -q "compose stop" "$work/log"'

setup ghcr.io/acme/tether:edge
mv "$work/deploy/.env" "$work/deploy/env.file" && ln -s env.file "$work/deploy/.env"
run 'id=0123456789abcdef
action=upgrade
tag=1.2.0'
check "a symlinked .env is left alone" '[ "$(state)" = failed ] && [ -L "$work/deploy/.env" ] && ! grep -q "compose pull" "$work/log"'

setup ghcr.io/acme/tether:edge
run 'id=0123456789abcdef
action=upgrade
tag=1.2.0' FAIL_PULL=1
check "a failed pull puts .env back" '[ "$(state)" = failed ] && [ "$(image)" = ghcr.io/acme/tether:edge ] && ! grep -q "up -d" "$work/log"'

setup ghcr.io/acme/tether:edge
run 'id=0123456789abcdef
action=upgrade
tag=edge' PULLED="$A"
check "the same image is already up to date" '[ "$(state)" = done ] && grep -q "Already up to date" "$work/status/status.json" && ! grep -q "up -d" "$work/log"'

setup ghcr.io/acme/tether:edge
run 'id=0123456789abcdef
action=upgrade
tag=1.2.0' RUNNING="false true"
check "a new version that keeps restarting fails, with rollback ready" '[ "$(state)" = failed ] && [ -s "$work/status/previous" ]'

setup tether:local
run 'id=0123456789abcdef
action=upgrade
tag=1.2.0'
check "a build from source upgrades on the server" '[ "$(state)" = failed ] && grep -q "from source" "$work/status/status.json" && [ ! -s "$work/log" ]'

setup ghcr.io/acme/tether:edge
ln -s "$work/deploy/.env" "$work/requests/request.link"
mv "$work/requests/request.link" "$work/requests/request"
env PATH="$work/bin:$PATH" LOG="$work/log" TETHER_DEPLOY_DIR="$work/deploy" REQUESTS_DIR="$work/requests" \
    STATUS_DIR="$work/status" TETHER_UID="$(id -u)" UPDATER_ONCE=1 sh "$src/updater.sh" > "$work/out" 2>&1 || true
check "a symlinked request is dropped unread" '[ ! -e "$work/requests/request" ] && [ -f "$work/deploy/.env" ] && [ ! -e "$work/status/status.json" ]'

[ "$failures" = 0 ] || { echo "$failures failed"; exit 1; }
echo "all passed"
