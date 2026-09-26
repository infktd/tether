# syntax=docker/dockerfile:1

# Builds natively for the host architecture (amd64 or arm64); CI runs one
# build per architecture on native runners rather than cross-compiling.
# Keep this tag in step with rust-toolchain.toml.
FROM rust:1.98.1-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends mold \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# sqlx checks queries against the committed .sqlx/ data; no database needed.
ENV SQLX_OFFLINE=true
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p tether-server \
 && cp target/release/tether /usr/local/bin/tether

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
# Snapshots and nightly backups (encrypted); a volume in docker-compose.yml.
# A fresh named volume takes this directory's owner and mode.
ENV SNAPSHOT_DIR=/var/lib/tether/snapshots
USER tether
EXPOSE 8080
ENTRYPOINT ["tether"]
