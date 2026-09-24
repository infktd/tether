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
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin tether
COPY --from=build /usr/local/bin/tether /usr/local/bin/tether
USER tether
EXPOSE 8080
ENTRYPOINT ["tether"]
