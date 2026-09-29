#!/bin/sh
# Tether's updater: upgrades and rolls back the app when an admin asks from
# the console (System page). It runs in its own container (the `updater`
# service in docker-compose.yml) with the Docker socket, no network and no
# ports; the app never gets the socket.
#
# The app writes a request into the requests volume:
#
#   id=<hex>
#   action=upgrade | rollback
#   tag=X.Y.Z | X.Y | latest | edge      (upgrade)
#   snapshot=<snapshot file name>         (rollback, when the upgrade migrated)
#
# Everything in it is checked here, as if the app were hostile: only a
# published tag of the image deploy/.env already names can be pulled, and a
# rollback goes back only to the image this updater recorded before its last
# upgrade. What happened goes to the status volume, which the app mounts
# read-only.
#
# Upgrade: TETHER_IMAGE in .env moves to the tag, the image is pulled, and
# only the app is recreated (it snapshots and migrates on start, as ever).
# Rollback, as deploy/README.md's: stop the app, restore the snapshot the
# upgrade took (if it migrated), pin the previous image by digest, start.

set -u

deploy=${TETHER_DEPLOY_DIR:?TETHER_DEPLOY_DIR must be set}
requests=${REQUESTS_DIR:-/requests}
state=${STATUS_DIR:-/status}
# The app's user (deploy/Dockerfile), who writes requests.
app_uid=${TETHER_UID:-10001}
# How long a new app gets to be running steadily.
settle=${SETTLE_SECONDS:-60}
step=${STEP_SECONDS:-5}
poll=${POLL_SECONDS:-5}
# Tests: handle what's waiting once, then exit.
once=${UPDATER_ONCE:-}

id=
action=

# status STATE MESSAGE: what the console shows. Messages are this script's
# own words and checked values only.
status() {
    tmp=$(mktemp "$state/.status.XXXXXX") || return 0
    printf '{"id":"%s","action":"%s","state":"%s","message":"%s","at":"%s"}\n' \
        "$id" "$action" "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$tmp"
    chmod 0644 "$tmp"
    mv -f "$tmp" "$state/status.json"
}

log() {
    echo "updater: $*"
}

fail() {
    log "failed: $1"
    status failed "$1"
}

# A request's field: the first KEY= line.
field() {
    printf '%s\n' "$request" | sed -n "s/^$1=//p" | head -n 1
}

matches() {
    printf '%s' "$1" | grep -Eqx "$2"
}

env_image() {
    sed -n 's/^TETHER_IMAGE=//p' "$deploy/.env" | tail -n 1
}

# set_image IMAGE: TETHER_IMAGE in .env, rewritten in one rename with
# .env's owner and mode kept (as install.sh does).
set_image() {
    # A symlinked .env would be replaced by a file with the link's owner
    # and mode: only a plain file is rewritten.
    if [ ! -f "$deploy/.env" ] || [ -L "$deploy/.env" ]; then
        return 1
    fi
    tmp=$(mktemp "$deploy/.env.XXXXXX") || return 1
    awk -v value="$1" '
        BEGIN { done = 0 }
        index($0, "TETHER_IMAGE=") == 1 { if (!done) print "TETHER_IMAGE=" value; done = 1; next }
        { print }
        END { if (!done) print "TETHER_IMAGE=" value }
    ' "$deploy/.env" > "$tmp" || { rm -f "$tmp"; return 1; }
    # It holds every secret: never readable by others, whatever it was.
    if ! chown "$(stat -c '%u:%g' "$deploy/.env")" "$tmp" ||
        ! chmod "$(stat -c '%a' "$deploy/.env")" "$tmp" ||
        ! chmod go-rwx "$tmp" ||
        ! mv -f "$tmp" "$deploy/.env"; then
        rm -f "$tmp"
        return 1
    fi
}

# The image's repository: TETHER_IMAGE without its tag or digest.
repository() {
    repo=${1%@*}
    case ${repo##*/} in
        *:*) repo=${repo%:*} ;;
    esac
    printf '%s' "$repo"
}

# The running app's image, as REPOSITORY@sha256:..., or nothing.
running_digest() {
    cid=$(docker compose ps -q app 2>/dev/null | head -n 1)
    [ -n "$cid" ] || return 0
    img=$(docker inspect -f '{{.Image}}' "$cid" 2>/dev/null) || return 0
    image_digest "$img"
}

# image_digest IMAGE: its REPOSITORY@sha256:... for this install's
# repository, or nothing.
image_digest() {
    docker image inspect -f '{{range .RepoDigests}}{{println .}}{{end}}' "$1" 2>/dev/null |
        grep -F "$repo@sha256:" | head -n 1
}

# Whether the app has been running, and not restarting, for $settle
# seconds, looking every $step seconds for up to three times that.
settled() {
    steady=0
    tries=$((settle * 3 / (step > 0 ? step : 1) + 1))
    while [ "$tries" -gt 0 ]; do
        tries=$((tries - 1))
        sleep "$step"
        cid=$(docker compose ps -q app 2>/dev/null | head -n 1)
        run=
        [ -z "$cid" ] || run=$(docker inspect -f '{{.State.Running}} {{.State.Restarting}}' "$cid" 2>/dev/null)
        if [ "$run" = "true false" ]; then
            steady=$((steady + step))
            [ "$steady" -lt "$settle" ] || return 0
        else
            steady=0
        fi
    done
    return 1
}

upgrade() {
    tag=$(field tag)
    matches "$tag" '[0-9]{1,6}(\.[0-9]{1,6}){1,2}|latest|edge' || { fail "Not a published version."; return; }
    new=$repo:$tag
    before=$(running_digest)
    status running "Pulling $tag."
    log "upgrading to $new"
    set_image "$new" || { fail "Couldn't write deploy/.env."; return; }
    if ! docker compose pull app; then
        set_image "$current"
        fail "Couldn't pull $tag: is it published?"
        return
    fi
    after=$(image_digest "$new")
    if [ -n "$before" ] && [ "$before" = "$after" ]; then
        status "done" "Already up to date: $tag is the version running."
        return
    fi
    status running "Starting $tag: it takes a snapshot and migrates first."
    if ! docker compose up -d --no-deps app; then
        set_image "$current"
        docker compose up -d --no-deps app
        fail "Couldn't start $tag; the previous version is back."
        return
    fi
    # One step back: the image that ran before, by digest (edge moves),
    # and the one it went to, which must still run to go back from it.
    if [ -n "$before" ] && [ -n "$after" ]; then
        printf 'from=%s\nto=%s\n' "$before" "$after" > "$state/previous"
        chmod 0644 "$state/previous"
    else
        rm -f "$state/previous"
    fi
    if settled; then
        status "done" "Upgraded to $tag."
    else
        fail "$tag keeps restarting: see its logs, or roll back from the console."
    fi
}

rollback() {
    previous=$(sed -n 's/^from=//p' "$state/previous" 2>/dev/null | head -n 1)
    upgraded=$(sed -n 's/^to=//p' "$state/previous" 2>/dev/null | head -n 1)
    matches "$previous" '[A-Za-z0-9./_-]+@sha256:[0-9a-f]{64}' || { fail "Nothing to roll back to."; return; }
    [ "$(repository "$previous")" = "$repo" ] || { fail "Nothing to roll back to."; return; }
    # Only from the version the console upgraded to: if anything moved
    # Tether since, the snapshot and the version shown don't match it.
    if [ -z "$upgraded" ] || [ "$(running_digest)" != "$upgraded" ]; then
        fail "Tether changed outside the console since its upgrade: roll back on the server (deploy/README.md)."
        return
    fi
    snapshot=$(field snapshot)
    if [ -n "$snapshot" ]; then
        matches "$snapshot" '[A-Za-z0-9][A-Za-z0-9._-]{0,199}' || { fail "Not a snapshot name."; return; }
    fi
    status running "Stopping Tether."
    log "rolling back to $previous"
    docker compose stop app || { fail "Couldn't stop Tether."; return; }
    if [ -n "$snapshot" ]; then
        status running "Restoring the snapshot taken before the upgrade."
        if ! docker compose run --rm --no-deps app rollback --yes --snapshot "$snapshot"; then
            docker compose start app
            fail "Restoring the snapshot failed; Tether is running the upgraded version again."
            return
        fi
    fi
    set_image "$previous" || { docker compose start app; fail "Couldn't write deploy/.env."; return; }
    status running "Starting the previous version."
    if ! docker compose up -d --no-deps app; then
        fail "Couldn't start the previous version: run deploy/install.sh on the server."
        return
    fi
    rm -f "$state/previous"
    if settled; then
        status "done" "Rolled back to the previous version."
    else
        fail "The previous version keeps restarting: see its logs."
    fi
}

handle() {
    id=$(field id)
    action=$(field action)
    matches "$id" '[0-9a-f]{8,32}' || { id=; action=; fail "Unreadable request."; return; }
    case $action in
        upgrade|rollback) ;;
        *) action=; fail "Unreadable request."; return ;;
    esac
    current=$(env_image)
    repo=$(repository "$current")
    case $current in
        ''|tether:local|*/tether:local)
            fail "This install builds Tether from source: upgrade on the server with git pull and deploy/install.sh --build."
            return
            ;;
    esac
    matches "$repo" '[A-Za-z0-9.-]+(/[A-Za-z0-9._-]+)+' || { fail "deploy/.env's TETHER_IMAGE isn't a registry image."; return; }
    "$action"
}

cd "$deploy" || exit 1
[ -f .env ] || { log "no .env in $deploy"; exit 1; }
mkdir -p "$requests" "$state"
# The app writes requests; nobody else reads them.
chown "$app_uid:$app_uid" "$requests" && chmod 0700 "$requests"
# The updater's alone: the app mounts it read-only, and owns nothing in it.
chown 0:0 "$state" && chmod 0755 "$state"
log "watching for requests from the console"

while :; do
    date -u +%Y-%m-%dT%H:%M:%SZ > "$state/.alive.tmp" && mv -f "$state/.alive.tmp" "$state/alive"
    file=$requests/request
    if [ -L "$file" ]; then
        rm -f "$file"
    elif [ -f "$file" ]; then
        # A FIFO put there instead mustn't hang the updater.
        request=$(timeout 5 head -c 1024 -- "$file")
        rm -f "$file"
        handle
    fi
    [ -z "$once" ] || exit 0
    sleep "$poll"
done
