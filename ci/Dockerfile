# ttrss-ci — the CI job image (self-hosted runner optimization).
#
# The build job's container. Bakes in everything the workflow's install steps
# would otherwise unpack into the job container's writable layer on every run
# — the runner's disk is spinning rust, so unpack + dpkg-configure (not the
# download) is the per-run cost:
#   * the ensure-steps' apt set (node/npm, zstd, postgresql-client, iproute2),
#   * the docker client stack (official repo: docker-ce + buildx + compose),
#   * clippy + rustfmt,
#   * the Playwright side: e2e's node_modules (from the lock file) + chromium
#     and all of its apt dependencies.
#
# The workflow's ci-image job rebuilds this on every run; the daemon's layer
# cache makes unchanged layers near-free. `COPY e2e/package*.json` sits before
# the Playwright layer, so a lock-file change busts exactly that layer and a
# Playwright version bump re-bakes the matching chromium automatically — no
# manual step.
#
# The job verifies the baked node_modules against the checked-out lock files
# (pkg-hash below, same pipeline as the workflow's Playwright step) and reuses
# it through a symlink; a mismatch falls back to a real `npm ci`.
FROM rust:1.98-trixie

# The ensure-steps' apt set (docker comes from the official repo below).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl iproute2 nodejs npm postgresql-client zstd \
    && rm -rf /var/lib/apt/lists/*

# The fmt/clippy gates (the rust image ships neither).
RUN rustup component add clippy rustfmt

# The docker client stack (docs.docker.com/engine/install/debian): the job
# reaches the RUNNER's daemon through the mounted socket — no dockerd here.
RUN apt-get update \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
    && chmod a+r /etc/apt/keyrings/docker.asc \
    && printf 'Types: deb\nURIs: https://download.docker.com/linux/debian\nSuites: %s\nComponents: stable\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/docker.asc\n' "$(. /etc/os-release && echo "$VERSION_CODENAME")" "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/docker.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# Playwright: e2e's dependencies, then chromium + its apt deps. The package
# files are copied first so a lock-file change invalidates exactly this layer.
COPY e2e/package.json e2e/package-lock.json /opt/e2e/
WORKDIR /opt/e2e
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright
RUN npm ci \
    && npx playwright install --with-deps chromium \
    && sha256sum package.json package-lock.json | sha256sum | awk '{print $1}' > pkg-hash
