# Rust port — the ttrss image (drop-in replacement for the official PHP
# stack). Multi-stage:
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
# curl: the HEALTHCHECK probe (below).
# postgresql-client: `ttrss importdb` (pg_dump|pg_restore — the "try before
# you buy" import from an existing install); plain apt (no pgdg), same
# policy as the base image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 postgresql-client \
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
#
# TTRSS_DB_HOST/PORT mirror the reference image's own ENV defaults
# (tt-rss/.docker/app/Dockerfile): the image assumes the official stack
# topology (a peer service named "db"). TTRSS_PLUGINS deviates from the
# reference image's default ("auth_internal, note, nginx_xaccel"):
# nginx_xaccel is meaningless without nginx; the image ships
# auth_internal + tweak_article_header.
ENV TTRSS_DOCROOT=/var/www/html/tt-rss \
    TTRSS_LISTEN_ADDR=0.0.0.0:9000 \
    TTRSS_DATA_DIR=/var/lib/tt-rss \
    TTRSS_DB_HOST=db \
    TTRSS_DB_PORT=5432 \
    TTRSS_PLUGINS=auth_internal,tweak_article_header \
    RUST_LOG=info \
    OWNER_UID=1000 \
    OWNER_GID=1000
EXPOSE 9000
# Reference parity (official web-nginx image, identical flags): an HTTP
# probe of the real login page — 200 means the app serves at the configured
# base path (dead process / 404 / 500 => unhealthy). Deliberately weaker
# than the reference: our login page renders with the DB down (the reference
# 500s — session.rs TODO(scope)), so a DB outage does not flip this check.
# The URL is derived at check time from the container's own env: base path
# from TTRSS_SELF_URL_PATH (the same source of truth the router nests at),
# falling back to the app's own default base /tt-rss when it is unset —
# port from TTRSS_LISTEN_ADDR. Always dialed via localhost, never the
# user's external host, so no per-deployment wiring is needed.
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD ["sh", "-c", "s=${TTRSS_SELF_URL_PATH:-/tt-rss/}; case $s in */*) seg=${s#*/};; *) seg=;; esac; p=${TTRSS_LISTEN_ADDR##*:}; [ -n \"$p\" ] || p=9000; u=http://localhost:$p; [ -n \"$seg\" ] && u=$u/${seg%%/*}; curl --fail --silent --show-error $u/index.php"]
ENTRYPOINT ["ttrss"]
