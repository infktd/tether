# syntax=docker/dockerfile:1

# Builds natively for the host architecture (amd64 or arm64); CI runs one
# build per architecture on native runners rather than cross-compiling.
# Keep this tag in step with rust-toolchain.toml.
FROM rust:1.98.1-trixie AS build
# zip packages the bundled apps; wasm32-wasip2 is what they're built for.
RUN apt-get update \
 && apt-get install -y --no-install-recommends mold zip \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add wasm32-wasip2
WORKDIR /src
# sqlx checks queries against the committed .sqlx/ data; no database needed.
ENV SQLX_OFFLINE=true
COPY . .
# The server, then the apps that come with it (plugins/*), packaged
# unsigned: they ship in this image and are exactly as trusted as the
# binary (crates/web/src/bundled.rs).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p tether-server \
 && cp target/release/tether /usr/local/bin/tether \
 && scripts/bundle-apps.sh /usr/share/tether/apps

FROM debian:trixie-slim
# ca-certificates: rustls verifies ESI, SSO and Discord against the system
# trust store.
# postgresql-client-16: pg_dump, pg_restore and psql for snapshots and
# backups, matching the database's Postgres 16 (a newer client's restore
# scripts don't run on 16). Trixie ships 17, so it comes from the
# PostgreSQL project's apt repository, set up at build time only with the
# key postgresql-common carries; the running app never contacts it.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates postgresql-common \
 && /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y \
 && apt-get install -y --no-install-recommends postgresql-client-16 \
 && apt-get purge -y --auto-remove postgresql-common \
 && rm -f /etc/apt/sources.list.d/pgdg.sources /etc/apt/sources.list.d/pgdg.list \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin tether \
 && install -d -o tether -g tether -m 0700 /var/lib/tether /var/lib/tether/snapshots
COPY --from=build /usr/local/bin/tether /usr/local/bin/tether
# The apps that come with Tether, offered on the Apps page
# (BUNDLED_APPS_DIR defaults here).
COPY --from=build /usr/share/tether/apps /usr/share/tether/apps
# Snapshots and nightly backups (encrypted); a volume in docker-compose.yml.
# A fresh named volume takes this directory's owner and mode.
ENV SNAPSHOT_DIR=/var/lib/tether/snapshots
USER tether
EXPOSE 8080
ENTRYPOINT ["tether"]
