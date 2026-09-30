# Shadow-prod: test the Rust port against production data

Testing the Rust `ttrss` against a **copy** of a real production PHP tt-rss
database, locally, **without affecting production**. Goal: one `make` target
that (1) sets up a shadow Rust-backed instance, (2) copies the prod DB, and
(3) starts it up — **repeatable and efficient**, since this will be run many
times to build confidence in the port.

The value of the shadow (distinct from the oracle + unit/integration suites) is
**real production data**: the actual feed/article/user/pref rows, at prod's
volume, rather than the synthetic golden dataset. The oracle, the whole test
suite, and the shadow all run on Postgres 17, so the shadow is a like-for-like
environment with prod.

Everything runs in a **2-container docker compose stack** (`db` + `rust`) — no
PHP, no nginx, no updater. See "The stack" below.

## Verdict: viable, and cheap to do safely

- **Non-invasive copy.** `pg_dump` is read-only: it takes a consistent
  snapshot and only holds `ACCESS SHARE` locks (which never block writers).
  It never writes to production. This is the whole reason the "without
  affecting production" requirement is easy to meet.
- **The Rust server is a single self-contained process.** It serves the
  frontend static assets byte-for-byte *and* handles the dynamic routes
  (login, `do.php`/API, the SPA shell) in-process. Everything server-specific
  is env (`TTRSS_DB_*`, `TTRSS_SELF_URL_PATH`, `TTRSS_LISTEN_PORT`,
  `TTRSS_DOCROOT`, `TTRSS_CACHE_DIR`, `TTRSS_ENCRYPTION_KEY`,
  `TTRSS_DAEMON`). So "start everything" = **Postgres (shadow) + the server
  container**.
- **Tools are present:** local `pg_dump`/`psql`/`pg_restore` are **18.6**
  (newer than prod's 17, so they can dump it directly), and the base images
  (`rust:1.98-trixie`, `debian:trixie-slim`, `postgres:17-alpine`) are
  available.

## The stack: `db` + `rust` (no nginx, no PHP)

The PHP reference needs four containers: `db`, `app` (PHP-FPM), `web` (nginx:
serves static files + proxies `*.php` to PHP-FPM), and `updater` (the feed
daemon). The Rust port **collapses `web` + `app` into one self-contained
server** (static serving + all dynamic routes in-process), so the shadow is
just:

| service | image | role |
|---------|-------|------|
| `db` | `postgres:17-alpine` | the prod-data copy (host port `5433`) |
| `rust` | built from `shadow/Dockerfile` | the port; serves the app on container `:80` → host `:8282` |

No nginx, no PHP-FPM, no updater — the server runs with `TTRSS_DAEMON=0`, so
the prod snapshot stays frozen (no background feed updates during poking).

### Docker image (the `rust` service)

`shadow/Dockerfile`, multi-stage:

- **build** — `rust:1.98-trixie` compiles `target/release/ttrss` from `crates/`.
- **runtime** — `debian:trixie-slim` + the binary + the `tt-rss` frontend
  docroot (served byte-for-byte via `TTRSS_DOCROOT`).

Compiling inside the same debian family (trixie) as the runtime guarantees the
binary's glibc matches — no host-binary drop-in, no glibc surprises. First
`make shadow` builds it (slow, one-time); after that the build is cached and
reused.

## Recommended design

One entry point — **`make shadow`** — that runs, in order:

1. **Preflight.** Check `docker` + pg client tools; check `shadow/.env` (prod
   credentials) exists (else print a "create it from `shadow/.env.example`"
   error). **Drift check (hard-fail):** read prod's
   `SELECT schema_version FROM ttrss_version` and require it to equal the
   frozen reference's version (**151**); abort on any mismatch (or if the
   table is missing).
2. **Build.** `docker compose build rust` (cached when the source is unchanged).
3. **DB up.** `docker compose up -d db` — the `postgres:17-alpine` container on
   host port `5433`, wait for healthy.
4. **Reset.** Drop + recreate the `shadow` database (idempotent).
5. **Copy prod → shadow.** `pg_dump --format=custom --no-owner --no-acl` from
   prod (local 18.6 client) into a local dump, then `pg_restore --no-owner -d
   shadow` into the empty DB.
6. **Rust up.** `docker compose up -d rust` — connects to `db`, daemon off.
7. **Health check + summary** (curl the shadow's `/tt-rss/healthz` on `8282`;
   print URL, stop command).

### Companion targets

- **`make shadow-copy`** — re-sync only: reset `shadow`, re-dump prod → `shadow`,
  restart `rust` (no rebuild). The fast path for "I changed prod, give me a
  fresh copy."
- **`make shadow-stop`** — stop the `rust` + `db` containers (keeps the volume);
  the oracle is never touched.
- **`shadow-db`** (internal) — bring up `db` + reset the `shadow` database.

### Configuration

- **`shadow/.env`** (gitignored) holds the prod connection + options:
  `PROD_DB_HOST`, `PROD_DB_PORT`, `PROD_DB_USER`, `PROD_DB_NAME`, `PROD_DB_PASS`,
  plus the host ports `SHADOW_PORT` (default `8282`) and `SHADOW_DB_PORT`
  (default `5433`), and options that reuse the app's own names:
  `TTRSS_DB_USER` / `TTRSS_DB_PASS` / `TTRSS_DB_NAME`, `TTRSS_SELF_URL_PATH`,
  `TTRSS_ENCRYPTION_KEY`, and `RUST_LOG`.
- **`shadow/.env.example`** (committed) is the template with placeholders.
- Dump artifacts are transient (`/tmp`, deleted after the restore). Prod
  credentials are never committed.

## Efficiency & repeatability

- **Idempotent end-to-end:** each `make shadow` = stop prior shadow → build
  (cached) → db up → drop+recreate `shadow` → re-dump → rust up. Every run
  yields a fresh prod snapshot with no manual cleanup.
- **The container + image are reused** (only the `shadow` database is
  dropped/recreated each run, which is cheap and local).
- **`docker compose build rust`** is a no-op when the source is unchanged
  (layer caching).
- **`--format=custom`** = a compressed single file (smaller network transfer)
  and locally restorable with `pg_restore`; add `-j N` for a parallel restore on
  large DBs (mutually exclusive with `--single-transaction`).
- **Re-sync** via `make shadow-copy` skips the build entirely.

## Copy method — options

- **A (default): logical `pg_dump` + `pg_restore`.** Non-invasive, robust,
  cross-version. Recommended.
- **B: `--format=tar`** — another compressed single-file format; similar
  trade-offs to A.
- **C: `pg_basebackup` (physical base backup).** More efficient for *very*
  large DBs, but requires prod to allow replication (a replica role,
  `wal_level=replica`) and adds a standby connection to production — not in the
  "leave prod alone" spirit. Avoid unless the DB is huge.
- **D: incremental (watermark on changed rows).** Most efficient for repeat
  runs when prod is large and slow-changing, but complex (many tables, no
  single watermark to key off). Treat as a future optimization, not the default.
- **E: subset of tables** (skip history if unneeded). Risky (foreign keys); not
  the default.

## Risks / considerations

1. **tt-rss schema / app-version drift.** The Rust port targets the frozen
   reference (`sha-2f7d831`, schema version **151**). If production is a
   *different tt-rss version*, `ttrss_version.schema_version` will differ and
   the preflight **hard-fails** (the chosen policy). This is deliberate: it's
   cheaper to abort than to set up a shadow against a schema the port can't
   serve. To point the shadow at a different tt-rss version, bump the expected
   version constant after re-targeting the port.
2. **Disk + time.** A full logical copy each run. Check the prod size first
   (`SELECT pg_size_pretty(pg_database_size('<db>'))`); the custom dump is
   compressed. (Currently ~160 GB free locally.)
3. **Encryption key.** Not a concern for this prod (no username/password
   feeds). The server uses a default key; `TTRSS_ENCRYPTION_KEY` is available if that
   ever changes.
4. **Feed icons.** Prod's feed-icon files live on the prod webroot, not in the
   DB dump, so feed thumbnails may 404/default in the shadow. Cosmetic only —
   it does not affect the API/behavior the shadow is probing.
5. **Credentials hygiene.** Prod creds in gitignored `shadow/.env`; invoke
   `pg_dump`/`psql` with `PGPASSWORD` + `-w`; never committed.
6. **Network reachability.** Assumes the prod DB is reachable over TCP from
   this machine. If only reachable via a jump host, use an SSH tunnel
   (`ssh -L 15433:dbhost:5432`) or `ssh host pg_dump ...`.
7. **Prod is never written to.** The shadow server talks only to the local
   `shadow` DB; with the daemon off it performs no feed fetches at all.

## Make targets (proposed surface)

| target | action |
|---|---|
| `make shadow` | preflight + build + db + copy + rust + health (single entry point) |
| `make shadow-copy` | re-dump prod → `shadow` + restart `rust` (no rebuild) |
| `make shadow-stop` | stop the `rust` + `db` containers (keeps the volume) |
| `shadow-db` (internal) | ensure `db` is up + reset the `shadow` database |

New files: `shadow/Dockerfile`, `shadow/compose.yaml`, `shadow/.env.example`
(committed), `shadow/.env` (gitignored), `shadow/{preflight,copy,stop}.sh`, a
repo-root `.dockerignore`. `.gitignore` gains `shadow/.env` +
`shadow/feed-icons/`.

## Decisions (resolved)

1. **Shadow HTTP port** — `8282` (dedicated; coexists with the demo on `8281`).
2. **Encryption key** — not needed (no authenticated feeds in prod); default
   key, `TTRSS_ENCRYPTION_KEY` available as an override.
3. **Drift preflight** — **hard-fail** on a `ttrss_version.schema_version`
   mismatch (expected: 151).
4. **Stack** — 2-container compose (`db` + `rust`); the Rust port replaces
   nginx + PHP-FPM, so no nginx. Base images are debian (`rust:1.98-trixie` →
   `debian:trixie-slim`).
