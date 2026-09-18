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
RUN cargo build --release -p ttrss --bin ttrss

# --- runtime stage ---
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /opt/ttrss-web /opt/feed-icons
COPY --from=build /src/target/release/ttrss /usr/local/bin/ttrss
# The frontend docroot the Rust server serves byte-for-byte (frozen reference).
COPY tt-rss /opt/ttrss-web
# Server defaults; compose/env can override these.
ENV TTRSS_RUST_DOCROOT=/opt/ttrss-web \
    TTRSS_RUST_LISTEN=0.0.0.0:80 \
    TTRSS_DAEMON=0 \
    TTRSS_RUST_FEED_ICONS_DIR=/opt/feed-icons
EXPOSE 80
ENTRYPOINT ["ttrss"]
