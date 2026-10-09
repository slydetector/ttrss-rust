# Rust port — shadow image. Multi-stage:
#   build:   rust:1.98-trixie compiles the server from crates/ (trixie => glibc 2.41).
#   runtime: debian:trixie-slim (same debian family, so the binary's glibc matches)
#            + the binary + the tt-rss frontend docroot (served byte-for-byte).
# The Rust server is self-contained (static assets + dynamic routes in-process),
# so this single image replaces the PHP reference's nginx + PHP-FPM. No nginx.
ARG RUST_IMAGE=rust:1.98-trixie

# --- build stage ---
FROM ${RUST_IMAGE} AS build
WORKDIR /src
# Workspace manifests first (layer cache), then the crate sources.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Plugin asset files (JS/CSS) referenced by include_str! from the crate sources.
COPY tt-rss/plugins ./tt-rss/plugins
RUN cargo build --release -p ttrss --bin ttrss

# --- runtime stage ---
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/tt-rss
COPY --from=build /src/target/release/ttrss /usr/local/bin/ttrss
# The frontend docroot the Rust server serves byte-for-byte (frozen reference),
# at the familiar official path (docs/19).
COPY tt-rss /var/www/html/tt-rss
# Server defaults; compose/env can override these. Cache lives under the
# data dir by design (docs/19 volume model) — no TTRSS_CACHE_DIR override.
#
# 9000 = the reference's own app port (nginx -> app:9000); the process
# drops root before binding, so only unprivileged ports are supported.
# OWNER_UID/OWNER_GID = the reference image's `app` user defaults; the
# entrypoint (as root) chowns the data dir, then the server runs as this
# UID for its whole life (docs/19).
ENV TTRSS_DOCROOT=/var/www/html/tt-rss \
    TTRSS_LISTEN_ADDR=0.0.0.0:9000 \
    TTRSS_DATA_DIR=/var/lib/tt-rss \
    OWNER_UID=1000 \
    OWNER_GID=1000
EXPOSE 9000
ENTRYPOINT ["ttrss"]
