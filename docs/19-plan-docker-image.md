# Plan: Docker image — a 100% drop-in replacement for the official tt-rss images

**Date:** 2026-10-07 · **Status:** plan, pending sign-off on the decision items (§5)

## 1. The target

The official tt-rss Docker setup (per the tt-rss.org Installation Guide) is a
4-container stack built from **two** images:

| service            | image                          | command                   | role                                                                 |
| ------------------ | ------------------------------ | ------------------------- | -------------------------------------------------------------------- |
| `db`               | `postgres:17-alpine`           | —                         | the database (unchanged by us)                                        |
| `app`              | `supahgreg/tt-rss`             | *(default: `startup.sh`)* | rsyncs code to the shared volume, creates/migrates the schema, applies the admin-password policy, runs PHP-FPM (internal port 9000) |
| `updater`          | `supahgreg/tt-rss`             | `/opt/tt-rss/updater.sh`  | runs the multi-process feed-update daemon (`update_daemon2.php`)      |
| `web-nginx`        | `supahgreg/tt-rss-web-nginx`   | nginx                     | **port 80**; serves the volume under base `/tt-rss` (or site root via `APP_BASE=`), gzip level 6 |
| `backups` (option) | `supahgreg/tt-rss`             | `/opt/tt-rss/dcron.sh -f` | weekly `pg_dump` → `/backups/ttrss-backup-YYYYMMDD.sql.gz` + tar of `*.local`/config/feed-icons, 28-day retention |

Official images publish to **Docker Hub (`supahgreg/*`)** and **ghcr.io
(`tt-rss/*`)** for **linux/amd64 + linux/arm64**.

**Drop-in = one image of ours that replaces *both* official images.** The user
swaps the image names; compose, `.env`, and volumes stay byte-for-byte.

### The contract our image must honor

- **Env** — everything the official stack passes:
  - `TTRSS_DB_{HOST,PORT,USER,NAME,PASS}` (+ `TTRSS_DB_SSLMODE`), `TTRSS_SELF_URL_PATH`,
    and *any* `TTRSS_*` config override (the port already reads the same names —
    they come from PHP's `Config.php` env convention)
  - container-level: `ADMIN_USER_PASS`, `ADMIN_USER_ACCESS_LEVEL`,
    `AUTO_CREATE_USER{,_PASS,_ACCESS_LEVEL,_ENABLE_API}`, `OWNER_UID`/`OWNER_GID`,
    `APP_BASE` (site root vs `/tt-rss`), and the `VAR__FILE` docker-secret
    expansion the official entrypoints do
- **Commands** — default (the server), `/opt/tt-rss/updater.sh`,
  `/opt/tt-rss/dcron.sh -f`
- **Port 80**; **volumes** `app:/var/www/html`, `./config.d:/opt/tt-rss/config.d:ro`, `/backups`
- **Startup behavior** (from the official `startup.sh`, read line by line):
  wait for DB → `create extension if not exists pg_trgm` → schema create/migrate
  → admin-password policy (env value applied on **every** boot; else, if the
  password is still the stock `password`, generate a random 16-char password
  and print the star-banner to the logs) → auto-create the extra user → serve

## 2. Gap analysis

**Already in place:**

- `shadow/Dockerfile` — multi-stage `rust:1.98-trixie` → `debian:trixie-slim`,
  docroot baked in, port 80. The new image extends it; nothing is thrown away.
- The binary reads the official `TTRSS_*` env names; the in-process update
  daemon + cron housekeeping (`ttrss::updates::daemon_loop`) already *is* the
  updater service's job; password-hash formats match
  (`SHA1:<hex>`, `SHA1X` = `sha1("$salt:$pass")`); `/healthz` exists.
- The frozen reference ships the full v151 schema (`sql/pgsql/schema.sql`,
  which seeds `admin`/`password`, matching the official's
  `--user-check-password admin:password` flow) **and all 149 migration files**
  (`sql/pgsql/migrations/`).

**Missing (code):**

1. **No schema bootstrap/migration** — the port assumes a v151 DB exists.
   → new `ttrss init` subcommand: empty DB → run `schema.sql` (+ `pg_trgm`
   extension); DB below v151 → apply `migrations/<v>.sql` in ascending order
   (port of PHP's migration loop).
2. **No `update.php` user verbs** → port the six flags `init` needs:
   `user-check-password`, `user-set-password`, `user-set-access-level`,
   `user-add`, `user-exists`, `user-enable-api` (with test parity against the
   PHP CLI tests).
3. **No daemon-only mode** → new `ttrss daemon` subcommand (runs
   `daemon_loop`, no HTTP listener) — this is what `updater.sh` execs.
4. **Response compression** — the official nginx image gzips at level 6;
   our server currently sends everything uncompressed. Fix: tower-http
   `CompressionLayer` with **gzip + brotli + zstd** (no deflate — legacy).
   tower-http negotiates per request from `Accept-Encoding` (q-value aware,
   verified in the vendored 0.6.11 source), so 2026 browsers get zstd/br
   while mobile RSS apps, older browsers, and curl get exactly the official
   gzip-6, and clients advertising none of the three get the original body.
   Quality = tower-http's `Default` → each algorithm's native default:
   **gzip 6** (literally nginx's `gzip_comp_level 6`), **brotli 4**,
   **zstd 3** — the standard sweet spots, zero configuration. Implementation:
   `tower-http` (already in `Cargo.lock` transitively) becomes a direct dep
   of `crates/ttrss` with the three `compression-*` features, plus one
   `.layer(...)` in `build_router` and tests asserting the chosen
   `Content-Encoding` per `Accept-Encoding`. Drop-in-safe:
   `Content-Encoding` is transparent to the app — a strict superset of the
   official image's behavior.
5. *(optional)* **`APP_BASE`** — the official image controls the base path
   with it (empty = site root); we derive the base from
   `TTRSS_SELF_URL_PATH`. Honor `APP_BASE` when set.

## 3. Image design

```dockerfile
FROM debian:trixie-slim
  + ttrss binary
  + docroot baked at /var/www/html/tt-rss   ← official layout (shadow used /opt)
  + /opt/tt-rss/{updater.sh, dcron.sh, update.sh} shims
  + postgresql-client (pg_dump, pg_isready) + cron + ca-certificates
ENTRYPOINT:  shell script:
  VAR__FILE secret expansion → pg_isready wait → chown $OWNER_UID:$OWNER_GID
  → ttrss init   (idempotent: schema, migration, admin policy, auto-user)
  → exec ttrss   (server, 0.0.0.0:80)
HEALTHCHECK: http://localhost:80<base>/index.php   (official parity)
```

`TTRSS_PLUGINS` default: `auth_internal,note` (the official's `nginx_xaccel`
is dropped — we have no nginx). Result: **~60 MB image with no PHP, no nginx,
no rsync, no sudo** (the official app image is ~350 MB).

Service mapping for the user's existing compose, unchanged:

- `app` → our image, default command
- `updater` → our image, `command: /opt/tt-rss/updater.sh`
- `web-nginx` → our image, default command (serves port 80 itself)
- `backups` → our image, `command: /opt/tt-rss/dcron.sh -f`

## 4. Phases

- **P0 — code** (in-repo, tested): `ttrss init` (schema create/migrate + the
  six user verbs) + `ttrss daemon` subcommand + tests. The bulk of the work;
  everything after this is shell and Dockerfile.
- **P1 — image + fresh-install smoke**: build locally; the stock official
  compose pointing at our image; verify login as `admin`, feed add/update via
  the updater container, and a backup file after forcing the cron.
- **P2 — the real drop-in test**: run the *official* PHP stack for a few days
  (real DB, a few feeds, some config changes) → swap both image names to ours
  with the same volumes and `.env` → verify data, login, and updates. This is
  the bar "100% drop-in" means.
- **P3 — CI publishing**: buildx (amd64; arm64 via QEMU — decision 2),
  tag-triggered workflow → the registry(ies) chosen in decision 1. Reuses the
  existing runner and warm cargo cache.
- **P4 — docs**: README section + this file's deviations list, kept current:
  - no PHP CLI in-container (`docker compose exec app php84 …` is gone)
  - no xdebug / `TTRSS_XDEBUG_*`
  - `config.d/*.php` snippets are not applied (PHP-only; env covers everything we support)
  - `APP_UPSTREAM` / `APP_WEB_ROOT` are no-ops (no nginx)
  - no precompressed `.gz` assets (gzip/brotli/zstd at request time instead;
    modern browsers get *better* compression than the official image)
  - version string pinned to the frozen reference (`26.08-2f7d831f`)

## 5. Decision items

1. **Image name + registry(ies)** — e.g. `ghcr.io/<owner>/ttrss`, Docker Hub
   `<owner>/ttrss`, `forge.example.com/packages`? (ghcr is free, anonymous-pull,
   and CI-native; recommended as the lead.)
2. **Arch** — amd64-only first, arm64 later; or both from day one (QEMU
   builds on the runner are slow).
3. **Single-container semantics** — recommendation: mirror PHP exactly —
   default command has the update daemon **off**, the `updater` service does
   the updating; single-container users set `TTRSS_DAEMON=1`. (Alternative:
   daemon on by default, accepting duplicate updaters in the stock
   3-container setup.)
4. **Tag scheme** — `latest` + version tags; which version string (frozen
   reference `26.08-2f7d831f`, or our own `v0.1.0`-style)?

**Estimate:** P0 is 1–2 focused sessions; P1 a day; P2 is patience; P3 a
half-day.
