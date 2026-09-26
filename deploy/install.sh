#!/bin/sh
# One-command install: writes deploy/.env if missing, sets up the reverse
# proxy you choose, then starts the stack. Safe to re-run: secrets in .env
# are never replaced, and the proxy settings change only when you ask.
#
# Usage: deploy/install.sh [options] [domain]
#
#   --proxy caddy|nginx|traefik|none
#       caddy    bundled Caddy with automatic HTTPS (the default)
#       nginx    your nginx on this host; writes a ready server block
#       traefik  your Traefik in Docker; labels on the app, no Caddy
#       none     your own proxy: the app on 127.0.0.1:PORT, no Caddy
#   --port PORT                 app port on 127.0.0.1 (nginx, none; 8080)
#   --traefik-network NAME      Docker network Traefik is on
#   --traefik-certresolver NAME Traefik's certificate resolver
#   --traefik-entrypoint NAME   Traefik's HTTPS entry point (websecure)
#   --install-nginx             put the server block into /etc/nginx, test
#                               and reload (needs root; asked otherwise)
#   --no-start                  write the configuration, don't start
#
# Asked interactively when a terminal is attached and a flag is missing.
# deploy/README.md describes each proxy.
set -eu

cd "$(dirname "$0")"

usage() {
    sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

secret() {
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n'
}

interactive() {
    [ -t 0 ]
}

# ask VAR "Question" default: reads an answer, or the default on Enter.
ask() {
    printf '%s [%s]: ' "$2" "$3"
    read -r answer || answer=
    eval "$1=\${answer:-\$3}"
}

# The value of KEY in .env, or nothing.
get_var() {
    [ -f .env ] || return 0
    sed -n "s/^$1=//p" .env | tail -n 1
}

# A copy of .env being rewritten (it holds every secret): removed on exit.
tmp=
trap 'rm -f "$tmp"' EXIT

# Sets KEY=VALUE in .env, replacing an existing line or adding one. Only for
# the proxy settings; secrets are written once and never touched again.
# The new file (0600, from mktemp) replaces the old in one rename, so .env
# is never half-written.
set_var() {
    tmp=$(mktemp .env.XXXXXX)
    awk -v key="$1" -v value="$2" '
        BEGIN { done = 0 }
        index($0, key "=") == 1 { if (!done) print key "=" value; done = 1; next }
        { print }
        END { if (!done) print key "=" value }
    ' .env > "$tmp"
    mv -f "$tmp" .env
    tmp=
}

unset_var() {
    tmp=$(mktemp .env.XXXXXX)
    awk -v key="$1" 'index($0, key "=") != 1' .env > "$tmp"
    mv -f "$tmp" .env
    tmp=
}

# These values end up in .env, nginx configuration and Traefik rules: plain
# names only, so nothing can smuggle in a line or a directive. `case`
# patterns match the whole value, newlines included.
valid_domain() {
    case $1 in
        ''|*[!A-Za-z0-9.-]*|[.-]*|*[.-]) return 1 ;;
    esac
    [ "${#1}" -le 253 ]
}

valid_name() {
    case $1 in
        ''|*[!A-Za-z0-9_.-]*|[_.-]*) return 1 ;;
    esac
    [ "${#1}" -le 128 ]
}

valid_port() {
    case $1 in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "${#1}" -le 5 ] && [ "$1" -ge 1 ] && [ "$1" -le 65535 ]
}

proxy= port= traefik_network= traefik_certresolver= traefik_entrypoint=
install_nginx=no start=yes domain_arg= configure=no
while [ $# -gt 0 ]; do
    case $1 in
        --proxy) [ $# -ge 2 ] || die "$1 needs a value"; proxy=$2; configure=yes; shift 2 ;;
        --proxy=*) proxy=${1#*=}; configure=yes; shift ;;
        --port) [ $# -ge 2 ] || die "$1 needs a value"; port=$2; configure=yes; shift 2 ;;
        --port=*) port=${1#*=}; configure=yes; shift ;;
        --traefik-network) [ $# -ge 2 ] || die "$1 needs a value"; traefik_network=$2; configure=yes; shift 2 ;;
        --traefik-network=*) traefik_network=${1#*=}; configure=yes; shift ;;
        --traefik-certresolver) [ $# -ge 2 ] || die "$1 needs a value"; traefik_certresolver=$2; configure=yes; shift 2 ;;
        --traefik-certresolver=*) traefik_certresolver=${1#*=}; configure=yes; shift ;;
        --traefik-entrypoint) [ $# -ge 2 ] || die "$1 needs a value"; traefik_entrypoint=$2; configure=yes; shift 2 ;;
        --traefik-entrypoint=*) traefik_entrypoint=${1#*=}; configure=yes; shift ;;
        --install-nginx) install_nginx=yes; shift ;;
        --no-start) start=no; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) [ -z "$domain_arg" ] || die "one domain only"; domain_arg=$1; shift ;;
    esac
done

# Flags are checked before anything is written.
case $proxy in
    ''|caddy|nginx|traefik|none) ;;
    *) die "--proxy must be caddy, nginx, traefik or none, not '$proxy'" ;;
esac
[ -z "$port" ] || valid_port "$port" ||
    die "the port must be a number from 1 to 65535, not '$port'"
for name in "$traefik_network" "$traefik_certresolver" "$traefik_entrypoint"; do
    [ -z "$name" ] || valid_name "$name" || die "'$name' isn't a valid Traefik or Docker name"
done
[ -z "$domain_arg" ] || valid_domain "$domain_arg" ||
    die "'$domain_arg' isn't a valid domain name"

if [ -f .env ]; then
    fresh=no
    echo "Using existing deploy/.env"
    domain=$(get_var DOMAIN)
    if [ -n "$domain_arg" ] && [ "$domain_arg" != "$domain" ]; then
        echo "Keeping DOMAIN=$domain from deploy/.env (edit it there to change it)."
    fi
    valid_domain "$domain" || die "DOMAIN in deploy/.env ('$domain') isn't a valid domain name"
else
    # Written once everything is settled, below.
    fresh=yes
    domain=$domain_arg
    if [ -z "$domain" ]; then
        printf 'Domain for this instance (e.g. auth.example.com): '
        read -r domain || domain=
    fi
    if [ -z "$domain" ]; then
        echo "A domain is required." >&2
        exit 1
    fi
    valid_domain "$domain" || die "'$domain' isn't a valid domain name"
fi

# The proxy: a flag, else what .env says, else ask (new installs), else
# Caddy. Installs from before the choice existed ran Caddy, and keep it.
current=$(get_var TETHER_PROXY)
if [ -z "$current" ]; then
    configure=yes
fi
if [ -z "$proxy" ]; then
    proxy=$current
fi
if [ -z "$proxy" ]; then
    proxy=caddy
    if [ "$fresh" = yes ] && interactive; then
        echo
        echo "Reverse proxy (HTTPS in front of Tether):"
        echo "  caddy    bundled, gets Let's Encrypt certificates itself (recommended)"
        echo "  nginx    nginx already on this server; a server block is written for it"
        echo "  traefik  Traefik already running in Docker"
        echo "  none     your own proxy, pointed at 127.0.0.1"
        ask proxy "Which one" caddy
        case $proxy in
            caddy|nginx|traefik|none) ;;
            *) die "choose caddy, nginx, traefik or none" ;;
        esac
    fi
fi
case $proxy in
    caddy|nginx|traefik|none) ;;
    *) die "TETHER_PROXY in deploy/.env must be caddy, nginx, traefik or none, not '$proxy'" ;;
esac

# Everything is worked out and checked first, then written together, so a
# mistake leaves .env as it was.
if [ "$configure" = yes ]; then
    case $proxy in
        nginx|none)
            [ -n "$port" ] || port=$(get_var TETHER_PORT)
            if [ -z "$port" ]; then
                port=8080
                if interactive; then
                    ask port "Port for the app on 127.0.0.1" 8080
                fi
            fi
            valid_port "$port" || die "the port must be a number from 1 to 65535, not '$port'"
            ;;
        traefik)
            [ -n "$traefik_network" ] || traefik_network=$(get_var TRAEFIK_NETWORK)
            [ -n "$traefik_certresolver" ] || traefik_certresolver=$(get_var TRAEFIK_CERTRESOLVER)
            [ -n "$traefik_entrypoint" ] || traefik_entrypoint=$(get_var TRAEFIK_ENTRYPOINT)
            if interactive; then
                [ -n "$traefik_network" ] ||
                    ask traefik_network "Docker network Traefik is attached to" traefik
                [ -n "$traefik_certresolver" ] ||
                    ask traefik_certresolver "Traefik certificate resolver name" letsencrypt
                [ -n "$traefik_entrypoint" ] ||
                    ask traefik_entrypoint "Traefik HTTPS entry point" websecure
            fi
            [ -n "$traefik_entrypoint" ] || traefik_entrypoint=websecure
            [ -n "$traefik_network" ] || die "--traefik-network is required with --proxy traefik"
            [ -n "$traefik_certresolver" ] || die "--traefik-certresolver is required with --proxy traefik"
            for name in "$traefik_network" "$traefik_certresolver" "$traefik_entrypoint"; do
                valid_name "$name" || die "'$name' isn't a valid Traefik or Docker name"
            done
            ;;
    esac
fi
if [ "$proxy" = traefik ] && [ "$start" = yes ]; then
    network=${traefik_network:-$(get_var TRAEFIK_NETWORK)}
    docker network inspect "$network" >/dev/null 2>&1 ||
        die "Docker network '$network' doesn't exist. Use the one Traefik is attached to (--traefik-network), or create it and attach Traefik to it."
fi

if [ "$fresh" = yes ]; then
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

if [ "$configure" = yes ]; then
    case $proxy in
        caddy)
            set_var TETHER_PROXY caddy
            unset_var COMPOSE_FILE
            ;;
        nginx|none)
            set_var TETHER_PROXY "$proxy"
            set_var TETHER_PORT "$port"
            set_var COMPOSE_FILE docker-compose.yml:docker-compose.host-proxy.yml
            ;;
        traefik)
            set_var TETHER_PROXY traefik
            set_var TRAEFIK_NETWORK "$traefik_network"
            set_var TRAEFIK_CERTRESOLVER "$traefik_certresolver"
            set_var TRAEFIK_ENTRYPOINT "$traefik_entrypoint"
            set_var COMPOSE_FILE docker-compose.yml:docker-compose.traefik.yml
            ;;
    esac
    echo "Reverse proxy: $proxy (in deploy/.env)"
    if [ "$current" = nginx ] && [ "$proxy" != nginx ]; then
        echo "Leaving nginx: take Tether's server block out of nginx yourself, or it"
        echo "keeps proxying $domain to 127.0.0.1:$(get_var TETHER_PORT) (and holding ports 80 and 443):"
        echo "  sudo rm /etc/nginx/sites-enabled/tether-$domain.conf /etc/nginx/sites-available/tether-$domain.conf"
        echo "  (or /etc/nginx/conf.d/tether-$domain.conf), then: sudo nginx -t && sudo systemctl reload nginx"
    fi
fi
port=$(get_var TETHER_PORT)
[ "$proxy" = caddy ] || [ "$proxy" = traefik ] || valid_port "$port" ||
    die "TETHER_PORT in deploy/.env must be a number from 1 to 65535, not '$port'"

# nginx: a server block for this domain, next to .env. With a certificate
# already in certbot's usual place it serves HTTPS; without, it serves
# HTTP until `certbot --nginx` adds the certificate and a redirect.
nginx_conf=nginx-$domain.conf
write_nginx() {
    cert=/etc/letsencrypt/live/$domain
    {
        cat <<NGINX
# Tether at https://$domain: the nginx server block written by
# deploy/install.sh (--proxy nginx). Regenerated on each run; the copy in
# /etc/nginx is only replaced when it's unchanged.
#
# TLS is yours, outside Tether. Tether sends its own security headers
# (Content-Security-Policy, Strict-Transport-Security, X-Frame-Options,
# X-Content-Type-Options, Referrer-Policy): don't add them here too.
NGINX
        plain_http=no
        if [ -f "$cert/fullchain.pem" ]; then
            cat <<NGINX

server {
    listen 80;
    listen [::]:80;
    server_name $domain;
    location / {
        return 301 https://\$host\$request_uri;
    }
}

server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name $domain;

    ssl_certificate $cert/fullchain.pem;
    ssl_certificate_key $cert/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
NGINX
        else
            plain_http=yes
            cat <<NGINX
#
# No certificate for $domain yet. Until there is one, this block only
# listens on HTTP and refuses to pass anything on over it (the setup token
# must never cross the network in the clear), while certbot's challenge
# still works. Get the certificate with:
#   sudo certbot --nginx -d $domain
# which adds HTTPS to this block and moves HTTP into a redirect.

server {
    listen 80;
    listen [::]:80;
    server_name $domain;
NGINX
        fi
        for location in "/" "= /notifications/stream"; do
            echo
            if [ "$location" = "/" ]; then
                cat <<NGINX
    # App packages uploaded on the Apps page: up to 40 MiB and a signature.
    client_max_body_size 41m;

NGINX
            else
                cat <<NGINX
    # Server-sent events (the notification bell): passed on as they come,
    # and open for up to an hour (Tether sends a keep-alive every 5 s).
NGINX
            fi
            echo "    location $location {"
            if [ "$plain_http" = yes ]; then
                cat <<NGINX
        # HTTPS only; certbot's challenge location is an exact match and
        # wins over this one.
        if (\$scheme = http) {
            return 403 "Tether needs HTTPS first: sudo certbot --nginx -d $domain\n";
        }
NGINX
            fi
            cat <<NGINX
        proxy_pass http://127.0.0.1:$port;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host \$host;
        # The address nginx saw, replacing whatever the client sent: Tether
        # rate-limits by it.
        proxy_set_header X-Forwarded-For \$remote_addr;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header X-Forwarded-Host \$host;
NGINX
            if [ "$location" != "/" ]; then
                cat <<NGINX
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 1h;
NGINX
            fi
            echo "    }"
        done
        echo "}"
    } > "$nginx_conf"
}

# Copies the server block into /etc/nginx, checks it and reloads nginx.
# Leaves a changed copy alone, and takes its own copy out again if nginx
# rejects it.
install_nginx_conf() {
    if [ -d /etc/nginx/sites-available ] && [ -d /etc/nginx/sites-enabled ]; then
        target=/etc/nginx/sites-available/tether-$domain.conf
        link=/etc/nginx/sites-enabled/tether-$domain.conf
    elif [ -d /etc/nginx/conf.d ]; then
        target=/etc/nginx/conf.d/tether-$domain.conf
        link=
    else
        echo "No /etc/nginx/sites-available or /etc/nginx/conf.d here; put deploy/$nginx_conf where your nginx includes it."
        return 0
    fi
    # What this run added, so a rejected configuration can be taken out.
    added_file= added_link=
    if [ -f "$target" ]; then
        if cmp -s "$nginx_conf" "$target"; then
            echo "$target is already up to date."
        else
            echo "$target exists and differs (certbot or you changed it); leaving it alone."
            echo "Compare it with deploy/$nginx_conf."
            return 0
        fi
    else
        # (set -e doesn't apply in here: the caller tests the result.)
        cp "$nginx_conf" "$target" || return 1
        added_file=$target
        chmod 644 "$target" || { rm -f "$target"; return 1; }
    fi
    if [ -n "$link" ] && [ ! -e "$link" ] && [ ! -L "$link" ]; then
        ln -s "$target" "$link" || { rm -f "$added_file"; return 1; }
        added_link=$link
    fi
    if ! nginx -t; then
        [ -z "$added_link" ] || rm -f "$added_link"
        [ -z "$added_file" ] || rm -f "$added_file"
        if [ -n "$added_file$added_link" ]; then
            echo "nginx rejected the configuration; removed what this script added." >&2
        fi
        return 1
    fi
    if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet nginx; then
        systemctl reload nginx || return 1
    else
        nginx -s reload || return 1
    fi
    echo "Installed $target and reloaded nginx."
}

nginx_instructions() {
    echo "Install it into nginx (as root):"
    echo "  sudo cp deploy/$nginx_conf /etc/nginx/sites-available/tether-$domain.conf"
    echo "  sudo ln -s /etc/nginx/sites-available/tether-$domain.conf /etc/nginx/sites-enabled/"
    echo "  sudo nginx -t && sudo systemctl reload nginx"
    echo "(Without sites-available, copy it into /etc/nginx/conf.d/ instead.)"
    echo "Or re-run this script as root with --install-nginx."
}

if [ "$proxy" = nginx ]; then
    write_nginx
    echo "Wrote deploy/$nginx_conf"
fi

# Only COMPOSE_FILE from .env decides which files apply.
unset COMPOSE_FILE

if [ "$start" = yes ]; then
    if [ "$proxy" != caddy ]; then
        # Caddy from an earlier choice would still hold ports 80 and 443.
        docker compose --profile caddy rm -sf caddy >/dev/null 2>&1 || true
    fi
    docker compose up -d --build
fi

if [ "$proxy" = nginx ]; then
    echo
    if [ "$(id -u)" = 0 ] && command -v nginx >/dev/null 2>&1; then
        if [ "$install_nginx" = no ] && interactive; then
            ask reply "Install the server block into /etc/nginx and reload nginx? (y/n)" y
            case $reply in [Yy]*) install_nginx=yes ;; esac
        fi
        if [ "$install_nginx" = yes ]; then
            install_nginx_conf || nginx_instructions
        else
            nginx_instructions
        fi
    else
        [ "$install_nginx" = no ] || echo "--install-nginx needs root and nginx on this host."
        nginx_instructions
    fi
    if [ ! -f "/etc/letsencrypt/live/$domain/fullchain.pem" ]; then
        echo
        echo "Then get a certificate (yours, outside Tether), e.g. with certbot:"
        echo "  sudo certbot --nginx -d $domain"
    fi
fi

token=$(get_var SETUP_TOKEN)
echo
case $proxy in
    caddy)
        echo "Tether is starting. Caddy gets the certificate by itself." ;;
    nginx)
        echo "Tether is starting on 127.0.0.1:$port, behind nginx." ;;
    traefik)
        echo "Tether is starting behind Traefik (network $(get_var TRAEFIK_NETWORK), resolver $(get_var TRAEFIK_CERTRESOLVER))." ;;
    none)
        echo "Tether is starting on http://127.0.0.1:$port. Point your reverse proxy at it,"
        echo "serving https://$domain; deploy/README.md lists what it must do." ;;
esac
[ "$start" = yes ] || echo "(Not started: --no-start. Start it with: cd deploy && docker compose up -d --build)"
echo "Finish setup in your browser:"
echo "  https://$domain/"
echo "Setup token: $token"
