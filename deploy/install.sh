#!/bin/sh
# One-command install: writes deploy/.env if missing, then starts the stack.
# Usage: deploy/install.sh [domain]
set -eu

cd "$(dirname "$0")"

secret() {
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n'
}

if [ -f .env ]; then
    echo "Using existing deploy/.env"
else
    domain=${1:-}
    if [ -z "$domain" ]; then
        printf 'Domain for this instance (e.g. auth.example.com): '
        read -r domain
    fi
    if [ -z "$domain" ]; then
        echo "A domain is required." >&2
        exit 1
    fi
    umask 077
    cat > .env <<ENV
DOMAIN=$domain
POSTGRES_PASSWORD=$(secret)
SETUP_TOKEN=$(secret)
ENCRYPTION_KEY=$(secret)
ENV
    echo "Wrote deploy/.env"
fi

# Installs from before the token vault lack a key: add one, never replace.
if ! grep -q '^ENCRYPTION_KEY=' .env; then
    printf 'ENCRYPTION_KEY=%s\n' "$(secret)" >> .env
    echo "Added ENCRYPTION_KEY to deploy/.env"
fi

docker compose up -d --build

domain=$(sed -n 's/^DOMAIN=//p' .env)
token=$(sed -n 's/^SETUP_TOKEN=//p' .env)
echo
echo "Tether is starting. Finish setup in your browser:"
echo "  https://$domain/"
echo "Setup token: $token"
