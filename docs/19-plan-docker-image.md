# Plan: Docker image — a drop-in replacement for the official tt-rss images

**Date:** 2026-10-07 · **Status:** plan, pending sign-off on the decision items (§5)

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
- **Port 80**; **volume** — one: the cache dir (`/cache`, pinned via
  `TTRSS_CACHE_DIR=/cache` in the image) so feed icons and image caches
  survive image updates.
  The official's `app:/var/www/html` code+cache volume shrinks to this
  (code is baked into our image); `config.d` drops (see deviations)
- **Startup behavior** (from the official `startup.sh`, read line by line):
  wait for DB → `create extension if not exists pg_trgm` → schema create/migrate
  → admin-password policy (env value applied on **every** boot; else, if the
  password is still the stock `password`, generate a random 16-char password
  and print the star-banner to the logs) → auto-create the extra user → serve
  (update daemon on by default — the single container updates itself)

## 2. Gap analysis

**Already in place:**

- `shadow/Dockerfile` — multi-stage `rust:1.98-trixie` → `debian:trixie-slim`,
  docroot baked in, port 80. The new image extends it; nothing is thrown away.
- The binary reads the official `TTRSS_*` env names; the in-process update
  daemon + cron housekeeping (`ttrss::updates::daemon_loop`, on by default)
  *is* the updater service's job; password-hash formats match
  (`SHA1:<hex>`, `SHA1X` = `sha1("$salt:$pass")`); `/healthz` exists;
  response compression (gzip/brotli/zstd) is implemented.
- The frozen reference ships the full v151 schema (`sql/pgsql/schema.sql`,
  which seeds `admin`/`password`, matching the official's
  `--user-check-password admin:password` flow) **and all 149 migration files**
  (`sql/pgsql/migrations/`).

**Missing (code):**

1. **No schema bootstrap/migration** — the port assumes a v151 DB exists.
   → **designed in `docs/20-plan-schema-migration.md`**: built-in
   `schema::ensure_up_to_date` at every startup (SQL read from the docroot,
   port of PHP's `Db_Migrations`); the startup bootstrap reuses it alongside
   the user verbs below.
2. **No `update.php` user verbs** → port the six verbs
   (`user-check-password`, `user-set-password`, `user-set-access-level`,
   `user-add`, `user-exists`, `user-enable-api`) as library functions and
   wire them into the **in-process startup bootstrap** (env-driven, no
   separate CLI — see `docs/20` §4), with test parity against the PHP CLI
   tests.

## 3. Image design

```dockerfile
FROM debian:trixie-slim
  + ttrss binary
  + docroot baked at /var/www/html/tt-rss   (familiar official path; debug aid)
  + postgresql-client (pg_isready for the entrypoint's DB wait) + ca-certificates
  + ENV TTRSS_CACHE_DIR=/cache              (explicit; no CWD dependence)
  + /cache                                  (the cache-dir volume mount point)
ENTRYPOINT:  shell script:
  VAR__FILE secret expansion → pg_isready wait → chown $OWNER_UID:$OWNER_GID
  → exec ttrss   (startup runs the bootstrap in-process — schema per
                  docs/20, admin-password policy, auto-user, all env-driven —
                  then serves: server + daemon, 0.0.0.0:80)
HEALTHCHECK: http://localhost:80<base>/index.php   (official parity)
```

`TTRSS_PLUGINS` default: `auth_internal,note` (the official's `nginx_xaccel`
is dropped — we have no nginx). Result: **~60 MB image with no PHP, no nginx,
no rsync, no sudo, no cron** (the official app image is ~350 MB).

## 4. Phases

- **P0 — code** (in-repo, tested): the in-process startup bootstrap —
  schema create/migrate (**done, `docs/20`: `core::schema::ensure_up_to_date`
  runs at every startup, 15 DB/unit tests**) + the six user-verb behaviors
  (admin-password policy, access level, auto-user) as env-driven startup
  steps + tests. The remaining bulk; everything after this is shell and
  Dockerfile.
- **P1 — image + fresh-install smoke**: build locally; a two-service compose
  (`db` + our image) with a fresh postgres; verify login as `admin`, feed
  add, and the in-container daemon updating feeds on its own.
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
