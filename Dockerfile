# syntax=docker/dockerfile:1

# ---- Build stage ----
# Keep the Rust version in step with `rust-toolchain.toml`, which
# `.dockerignore` leaves out so rustup does not install another toolchain
# inside the build, and the Debian release in step with the runtime stage, so
# the binary links against the glibc it runs with.
FROM rust:1.94-slim-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# `xtask` is a workspace member, so cargo needs its sources to load the
# workspace; only `fluent-server` is built, and nothing of `xtask` ships.
COPY xtask ./xtask

# Cache mounts keep downloaded crates between local builds. The target
# directory is not cached: cargo would reuse a binary linked against another
# base image's glibc.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked -p fluent-server \
    && cp target/release/fluent /usr/local/bin/fluent

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 1000 --user-group --home-dir /data fluent \
    && mkdir -p /data/registrations \
    && chown -R fluent:fluent /data

COPY --from=builder /usr/local/bin/fluent /usr/local/bin/fluent

USER fluent
WORKDIR /data

# `/data` holds `fluent.toml`, the registration bundles and the SQLite store.
# The configuration must listen on `0.0.0.0:8080` (`[server].listen`, or
# `FLUENT_SERVER__LISTEN`) for the published port to reach the server.
VOLUME ["/data"]
EXPOSE 8080

CMD ["fluent", "serve", "--http", "--config", "/data/fluent.toml"]
