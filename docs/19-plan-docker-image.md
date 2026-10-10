# Plan: Docker image — a drop-in replacement for the official tt-rss images

**Date:** 2026-10-07 · **Status:** image built + shadow-verified (2026-10-08); blessed
layout landed (root `Dockerfile` + `docker/` stack, 2026-10-09); P1 fresh-install
smoke done (2026-10-10 — the `e2e/` Playwright golden path, in CI); P2 (the real
drop-in test) is the next gate

## 1. The target

The official tt-rss Docker setup (per the tt-rss.org Installation Guide) is a
multi-container stack built from **two** images:

| service            | image                          | command                   | role                                                                 |
| ------------------ | ------------------------------ | ------------------------- | -------------------------------------------------------------------- |
| `db`               | `postgres:17-alpine`           | —                         | the database                                                          |
| `app`              | `supahgreg/tt-rss`             | *(default: `startup.sh`)* | rsyncs code to the shared volume, creates/migrates the schema, applies the admin-password policy, runs PHP-FPM (internal port 9000) |
| `updater`          | `supahgreg/tt-rss`             | `/opt/tt-rss/updater.sh`  | runs the multi-process feed-update daemon (`update_daemon2.php`)      |
| `web-nginx`        | `supahgreg/tt-rss-web-nginx`   | nginx                     | **port 80**; serves the volume under base `/tt-rss` (or site root via `APP_BASE=`), gzip level 6 |
| `backups` (option) | `supahgreg/tt-rss`             | `/opt/tt-rss/dcron.sh -f` | weekly `pg_dump` + tar of `*.local`/config/feed-icons, 28-day retention |

Official images publish to **Docker Hub (`supahgreg/*`)** and **ghcr.io
(`tt-rss/*`)** for **linux/amd64 + linux/arm64**.

The `app` + `updater` + `web-nginx` split exists because the PHP version needs
three processes (FPM, a separate daemon, a web server) and Docker maps that to
containers. The Rust port is a **single binary that already is all three**: it
serves port 80 itself (static + dynamic, with gzip/brotli/zstd response
compression — implemented) and runs the feed-update daemon + cron
housekeeping in-process (`daemon_loop`, on by default).

**Drop-in = one image of ours that replaces the `app` + `updater` +
`web-nginx` trio with a *single* container.** The user's compose shrinks:

```yaml
services:
  db:
    image: postgres:17-alpine          # unchanged
    # ...

  ttrss:
    image: <ours>                      # replaces app + updater + web-nginx
    env_file: [.env]                   # same TTRSS_* / ADMIN_USER_* / AUTO_CREATE_USER_* vars
    ports: ["${HTTP_PORT}:80"]
    volumes:
      - cache:/cache             # the only user-facing mount (see below)
    depends_on: [db]

volumes:
  db:
  cache:
```

Same database, same `.env`, same URLs — three containers become one.

**Out of scope:** the `backups` service (deprioritized — it's a `pg_dump`
one-liner that can be run outside the image; possible follow-up).

### The contract our image must honor

- **Env** — everything the official `app` service consumed:
  - `TTRSS_DB_{HOST,PORT,USER,NAME,PASS}` (+ `TTRSS_DB_SSLMODE`),
    `TTRSS_SELF_URL_PATH`, and *any* `TTRSS_*` config override (the port
    already reads the same names — they come from PHP's `Config.php` env
    convention)
  - container-level: `ADMIN_USER_PASS`, `ADMIN_USER_ACCESS_LEVEL`,
    `AUTO_CREATE_USER{,_PASS,_ACCESS_LEVEL,_ENABLE_API}`,
    `OWNER_UID`/`OWNER_GID`, and the `VAR__FILE` docker-secret expansion the
    official entrypoints do
  - `APP_BASE`/`APP_WEB_ROOT`/`APP_UPSTREAM` go away with web-nginx; the base
    path comes from `TTRSS_SELF_URL_PATH` (users who ran site-root must have
    already adjusted it per the official docs)
- **Commands** — the default only (no `updater.sh`/`dcron.sh` shims; the
  daemon runs inside the server process)
- **Port 9000** — the reference's own *app* port (its web-nginx image
  proxies `app:9000`); we break from serving it on the nginx side. Only
  unprivileged ports (>= 1024) are supported (see privileges below).
  **Volume** — one: the data dir (`/var/lib/tt-rss`, baked via
  `TTRSS_DATA_DIR`) so caches and user-supplied data survive image updates.
  The official's `app:/var/www/html` code+cache volume shrinks to this (code
  is baked into our image); `config.d` drops (see deviations)
- **Startup behavior** (from the official `startup.sh`, read line by line):
  DB readiness (deviation: no `pg_isready` wait loop — the pool's connect
  retries + a non-zero exit on timeout let the compose restart policy
  re-drive startup; see gap analysis) → `create extension if not exists
  pg_trgm` → schema create/migrate
  → admin-password policy (env value applied on **every** boot; else, if the
  password is still the stock `password`, generate a random 16-char password
  and print the star-banner to the logs) → auto-create the extra user → serve
  (update daemon on by default — the single container updates itself)

## 2. Gap analysis

**Already in place:**

- `Dockerfile` (repo root; was `shadow/Dockerfile` before the blessed layout) —
  multi-stage `rust:1.98-trixie` → `debian:trixie-slim`, docroot baked in,
  port 9000, in-process chown + privilege drop. The new image extends it;
  nothing is thrown away.
- The binary reads the official `TTRSS_*` env names; the in-process update
  daemon + cron housekeeping (`ttrss::updates::daemon_loop`, on by default)
  *is* the updater service's job; password-hash formats match
  (`SHA1:<hex>`, `SHA1X` = `sha1("$salt:$pass")`); `/healthz` exists;
  response compression (gzip/brotli/zstd) is implemented.
- The frozen reference ships the full v151 schema (`sql/pgsql/schema.sql`,
  which seeds `admin`/`password`, matching the official's
  `--user-check-password admin:password` flow) **and all 149 migration files**
  (`sql/pgsql/migrations/`).

**Missing (code): all landed.**

1. ~~**No schema bootstrap/migration**~~ → **done (`docs/20`)**:
   `core::schema::ensure_up_to_date` at every startup (fresh DB →
   `schema.sql` + stamp 151; older DB → numbered files; fail-closed),
   15 tests.
2. ~~**No `update.php` user verbs**~~ → **done (2026-10-14)**:
   `user_add`, `user_modify`, `user_has_password`, `user_exists`,
   `user_check_password`, `user_set_password`, `user_set_access_level`,
   `user_enable_api`, `user_remove` ported 1:1 into `ttrss::users`
   (`tests/user_verbs.rs`, 12 cases — no PHP CLI suite exists for these,
   so the port carries its own coverage, same bar as `Db_Migrations`).
3. **Startup bootstrap** → **done (2026-10-14, `ttrss::bootstrap`)**: the
   reference `startup.sh`'s user-management steps in-process —
   admin-password policy (stock `password` → random 16-char + the verbatim
   star banner), `ADMIN_USER_ACCESS_LEVEL` (invalid → fail-closed),
   `AUTO_CREATE_USER*`, and the post-init SQL patches
   (`<data_dir>/sql/post-init.d`, sorted, autocommit — the reference's
   `find … | psql -f` under `set -e`). 4 unit + 12 integration tests
   (`tests/bootstrap.rs`).

## 3. Image design

```dockerfile
FROM debian:trixie-slim
  + ttrss binary
  + docroot baked at /var/www/html/tt-rss   (read-only code; the familiar
                                              official path; debug aid)
  + ca-certificates
  + ENV TTRSS_DATA_DIR=/var/lib/tt-rss       (baked; the user never sees the knob)
  + ENV TTRSS_LISTEN_ADDR=0.0.0.0:9000
  + ENV OWNER_UID=1000 OWNER_GID=1000        (the reference image's defaults)
  + /var/lib/tt-rss                          (the single `app` volume mount point)
ENTRYPOINT: ["ttrss"]   (no shell — everything is in-process:
  VAR__FILE secret expansion → chown data dir to OWNER_UID:OWNER_GID (root
  only) → setgroups/setgid/setuid drop (irreversible) → schema bootstrap
  (docs/20), admin-password policy, access level, auto-user, post-init
  patches — all env-driven — then serve server + daemon on 0.0.0.0:9000,
  entirely as OWNER_UID)
HEALTHCHECK: http://localhost:9000<base>/index.php   (official parity)
```

**Privileges (non-root steady state):** the process starts root only so it
can chown the data volume; it then drops to `OWNER_UID`/`OWNER_GID`
(default 1000 — the reference image's `app` user) via
`setgroups`/`setgid`/`setuid` and never regains root (kernel-enforced).
The drop happens before the tokio runtime exists, so every worker thread is
born the app UID. Consequences and deviations from the reference:

- **Port**: 9000 (the reference app image's own port), unprivileged only —
  `TTRSS_LISTEN_ADDR` < 1024 fails with a clear error (the reference's
  nginx side served 80; we have no nginx, and no
  `CAP_NET_BIND_SERVICE` opt-in, deliberately).
- **Stronger than the reference**: its php-fpm *master* stays root
  permanently; ours is root only for the chown.
- **`OWNER_UID`/`OWNER_GID`**: unprefixed names on purpose (the reference's
  `ENV OWNER_UID=1000`), so a user's UID tuning carries over. Set them to
  the host user's UID to keep bind-mount ownership a no-op:
  `docker run -e OWNER_UID=$(id -u)` / compose `OWNER_UID: ${OWNER_UID:-1000}`.
  Ignored when the container starts non-root (`--user`), like the
  reference. `OWNER_UID=0` opts back into all-root.
- **chown is top-level only** (the reference does `chown -R` every boot):
  the app writes everything below as the app user, so the recursive sweep
  buys nothing under the fresh-volume contract.
- **DB readiness**: no `pg_isready` loop (the reference waits forever — its
  own `TODO` admits it should bail). The sqlx pool retries connects for its
  30 s `acquire_timeout`, then the process exits non-zero; the compose
  `restart: unless-stopped` re-drives startup. Same self-healing as the
  infinite loop, plus a visible crash loop (with the pool's error) when the
  DB never comes. Mid-run DB outages self-heal via the pool's background
  reconnect without a restart — something a one-shot entrypoint can't do.

**The single `app` volume** (the official docs' volume *name*, kept):
everything the app writes or the user supplies lives under one mount —
`/var/lib/tt-rss` (FHS: app state; the official's 7-volume layout collapses
to one because our code is baked, not rsync'd):

```
app → /var/lib/tt-rss
  cache/            feed-icons/, images/, …  (regenerable; TTRSS_CACHE_DIR
                                               still wins if explicitly set)
  sql/post-init.d/  user SQL patches, run at every boot
  themes.local/     user themes (served at the docroot-relative URL
                    themes.local/<t>; the static handler maps that prefix)
```

Deliberately *absent* (no code in the port reads them): `plugins.local/`
and `templates.local/` (plugins compiled in, templates embedded),
`lock/` (no cross-process lock files — one in-process daemon),
`config.php` (config is env-driven). Migration from the official image:
**keep the Postgres volume** (re-point the same `TTRSS_DB_*` — feeds,
articles, tags, users, prefs and even live sessions live in the DB); use a
**fresh app volume**. Not carried over: manually-set feed icons
(`cache/ln`) and user-local themes (re-add as wanted); everything else on
the official's volume is regenerable cache. (The official's `app` volume
mixes code and data because PHP needs a live working copy; ours is
data-only, so there is nothing else to copy.)

`TTRSS_PLUGINS` default: `auth_internal,note` (the official's `nginx_xaccel`
is dropped — we have no nginx). Result: **~60 MB image with no PHP, no nginx,
no rsync, no sudo, no cron** (the official app image is ~350 MB).

## 4. Phases

- **P0 — code: DONE (2026-10-14).** The in-process startup bootstrap —
  schema create/migrate (`docs/20`, 15 tests) + the user-verb functions
  (`ttrss::users`, 12 tests) + the env-driven **policy wiring**
  (`ttrss::bootstrap`: admin-password star-banner, access level,
  auto-user, post-init patches; 4 unit + 12 integration tests) + the
  directory model (`Config.data_dir` / `TTRSS_DATA_DIR`, `cache/` under
  it, `themes.local` served from it). Everything remaining is shell and
  Dockerfile.
- **P1 — image + fresh-install smoke: DONE (2026-10-10, `e2e/`).** The
  Playwright golden path is the smoke, automated and in CI (the `build`
  job, after `cargo test`): a fresh two-service stack from the blessed
  `docker/compose.yaml` (synthesized throwaway copy), a real browser walks
  boot → login (the star-banner admin password) → subscribe (the feed add)
  → the in-container daemon fetching the feed on its own (the 5 s
  `TTRSS_DAEMON_SLEEP_INTERVAL` test override) → opening the feed and
  reading an article in-app. The suite's setup also gates on the image's
  HEALTHCHECK reaching `healthy` before the test starts (a DB outage does
  *not* flip it — our login page renders without the DB (`session.rs`
  `TODO(scope)`; the reference 500s)). See `e2e/README.md`.
- **P2 — the real drop-in test**: run the *official* PHP stack (app +
  updater + web-nginx) for a few days (real DB, a few feeds, some config
  changes) → replace **all three** services with our single one (same `db`
  volume, same `.env`) → verify data, login, and updates. This is the bar
  "drop-in" means.
- **P3 — CI publishing**: buildx (amd64; arm64 via QEMU — decision 2),
  tag-triggered workflow → the registry(ies) chosen in decision 1. Reuses the
  existing runner and warm cargo cache.
- **P4 — docs**: README section + this file's deviations list, kept current:
  - three containers become one (`updater.sh`/`dcron.sh` commands don't exist)
  - no `backups` service (out of scope; `pg_dump` from anywhere)
  - no PHP CLI in-container (`docker compose exec app php84 …` is gone)
  - no xdebug / `TTRSS_XDEBUG_*`
  - no `config.d/*.php` snippets (PHP-only; the equivalent `TTRSS_*` env var
    covers everything they can set)
  - no `plugins.local`/`themes.local`/`templates.local` customization dirs
    (plugins are compiled-in Rust; `TTRSS_PLUGINS` selects them)
  - `APP_UPSTREAM` / `APP_WEB_ROOT` / `APP_BASE` are no-ops (no nginx)
  - no precompressed `.gz` assets (gzip/brotli/zstd at request time instead;
    modern browsers get *better* compression than the official image)
  - version string pinned to the frozen reference (`26.08-2f7d831f`)
  - in-container port is **9000** (compose mapping `…:9000`, was `…:80`)
  - the process runs as **`OWNER_UID`/`OWNER_GID`** (default 1000), root
    only for the data-dir chown; no `CAP_NET_BIND_SERVICE` / port-80 mode
  - no infinite DB wait at boot: a DB that never appears yields a crash
    loop (pool timeout → exit → compose restart) instead of a silent
    "waiting..." hang
  - **SVG favicons work** (extension): the reference's MIME allowlist
    (RSSUtils.php:9-15) predates SVG and its GD `calculate_avg_color` can't
    open one, so a feed serving an SVG icon gets the default gray. We allow
    `image/svg+xml` (serving is safe: it renders in `<img>` context, which
    sandboxes embedded scripts) and compute the accent color from the fill
    palette via `usvg` (parse + tree walk, **no rasterizer** — ~14 pure-Rust
    crates; a huge `viewBox` can't blow up memory the way a rasterizer
    pixmap could). Degenerate SVGs (strokes/filters/patterns only) still
    fall back to gray, matching the reference's behavior.

## 5. Decision items

1. **Image name + registry(ies)** — e.g. `ghcr.io/<owner>/ttrss`, Docker Hub
   `<owner>/ttrss`, `forge.example.com/packages`? (ghcr is free, anonymous-pull,
   and CI-native; recommended as the lead.)
2. **Arch** — amd64-only first, arm64 later; or both from day one (QEMU
   builds on the runner are slow).
3. **Tag scheme** — `latest` + version tags; which version string (frozen
   reference `26.08-2f7d831f`, or our own `v0.1.0`-style)?

**Estimate:** P0 is 1–2 focused sessions; P1 a day; P2 is patience; P3 a
half-day.
