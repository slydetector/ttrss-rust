# Oracle: dockerized PHP tt-rss + golden dataset + corpus

The oracle is a live, stock Tiny Tiny RSS (PHP) deployment that serves two
purposes:

1. **Reference implementation** for the tt-rust port: the harness records its
   HTTP behavior as fixtures (golden corpus), and the Rust server is diffed
   against it.
2. **Performance baseline**: `scripts/bench.sh` measures PHP-FPM latency for
   the key endpoints; the Rust server must beat it.

## Layout

```
oracle/
├── compose.yaml          # 4 services: db, app (php-fpm), updater, web (nginx)
├── .env                  # deterministic env (port, key, seed values)
├── feeds.txt             # the 41-feed subscription list (live + broken)
├── scripts/
│   ├── up.sh             # start stack + wait for healthz
│   ├── down.sh           # stop stack (keep volumes)
│   ├── seed.py           # full golden-dataset seed (users, feeds, state, keys)
│   ├── snapshot.sh       # pg_dump + app cache + manifest -> oracle/data/
│   ├── restore.sh        # restore golden-db.sql into a fresh db container
│   ├── make_manifest.py  # row counts / facts -> db-manifest.json
│   ├── bench.sh          # latency baseline -> oracle/data/baseline.json
│   └── census.sh         # nginx request-distribution summary
├── corpus/
│   └── scenarios.template.toml   # the 58-step record/replay scenario suite
└── data/                 # generated (gitignored)
    ├── golden-db.sql     # frozen full DB dump (the golden dataset)
    ├── cache/            # copied app cache (feed icons, cached feed XML)
    ├── db-manifest.json  # row counts + key facts
    ├── ids.json          # feed/category/user ids + credentials for the harness
    ├── baseline.json     # PHP-FPM latency baseline
    └── runs/<name>/      # harness fixtures: NNN_<step>.json
```

## Stack

- `db` — postgres:15-alpine, single database `postgres`.
- `app` — official tt-rss FPM image pinned to `sha-2f7d831`; shares a
  `webroot` volume with `web` and `updater` at `/var/www/html`.
- `updater` — `update_daemon2.php` in a loop; **stopped by the seed script** so
  the corpus runs against a frozen feed set. (Stopping it SIGKILLs the fork
  after the 10s grace period — exit 137 is expected, not a crash.)
- `web` — nginx image, standard deployment: app under `/tt-rss/`
  (`APP_BASE=/tt-rss`, `APP_WEB_ROOT=/var/www/html`).

Base URL: `http://127.0.0.1:8280/tt-rss/` (port from `.env`).

Key env: `TTRSS_ENCRYPTION_KEY` (session encryption active),
`TTRSS_SELF_URL_PATH=http://127.0.0.1:8280/tt-rss/`.

## Golden dataset

Produce from scratch:

```sh
oracle/scripts/up.sh
python3 oracle/scripts/seed.py      # ~3 min: fetches 41 feeds, seeds state
oracle/scripts/snapshot.sh          # golden-db.sql + cache + manifest
```

Seeded contents (stable by construction, ids in `oracle/data/ids.json`):

- 41 feeds (38 live + 3 intentionally broken), 9 categories (nested),
  ~3.9k entries, ~560 marked, ~540 notes, ~300 scores, ~3.7k tags,
  ~3k label assignments, ~2.7k enclosures, 8 labels, 2 filters.
- 3 users:
  - `admin/admin` — admin; **TOTP enabled** (secret `JBSWY3DPEHPK3PXP`,
    6-digit/30s/SHA-1; the column stores the ASCII secret, the effective
    HMAC key is its ASCII bytes); **app password** `AppPass123` for the API
    (service `_api`); TOTP + app-password + API-access prefs set.
  - `readonly/readpass` — read-only (no feeds of its own; useful for
    not-found / permission edge cases).
  - `testuser/testpass` — created via `update.php --user-add` (CLI path).
- 2 profiles (Home/Work), 2 access keys (feed `a1b2c3d4e5f6g`,
  category `g6d4c3b2a1e8f`), error-log entries from the broken feeds.
- Fixed `last_login` on all users so the users-admin page is stable across
  corpus runs.

The seed stops the updater daemon before recording feed state, so the
snapshot is frozen (no background feed updates during corpus runs).

## Corpus

`corpus/scenarios.template.toml` defines 58 ordered steps covering:

- public.php: healthz, login page, static assets
- web login (admin with OTP challenge + code), shell, prefs shell, ETag/304
- backend.php RPC: runtime info, counters, sanity check, updates, hotkeys
- Pref_Feeds: feed trees, feeds with errors, inactive feeds
- Feeds::view: unread / all / category headlines, search, update debugger
- Article: metadata, tags; Pref_Labels tree; Pref_Filters tree
- Pref_Users: index + details; Pref_System: scheduled tasks
- Pref_Prefs: profiles, plugins list/available, digest preview
- external JSON API (level 23): login (app password), isLoggedIn, config,
  unread, counters, feeds, categories, feed tree, labels, headlines (feed +
  category), article, getPref
- syndication: RSS/Atom with valid feed & category keys, bad key (403),
  cached file, feed icon
- logout; readonly user login + feed tree + headlines ("Feed not found")

Run it:

```sh
tt-rust-harness run --scenarios oracle/corpus/scenarios.template.toml \
    --vars oracle/data/ids.json --out oracle/data/runs/<name>
```

Variables like `{{totp:{{totp_secret}}}}` compute live TOTP codes. Steps
re-execute per run, so volatile session/CSRF/API-sid values are always
re-derived; `--vars` supplies the stable seeded facts.

## Proving the harness (self-replay)

Two runs minutes apart must diff clean after normalization:

```sh
tt-rust-harness run ... --out oracle/data/runs/oracle-a
tt-rust-harness run ... --out oracle/data/runs/oracle-b
tt-rust-harness diff --a oracle/data/runs/oracle-a --b oracle/data/runs/oracle-b
# -> 58 exchange(s): 58 match, 0 mismatch
```

Normalizations applied at diff time: CSRF tokens / session ids / sids dropped
from JSON; `recent_log_events` dropped (grows with request history);
ISO/RFC822/`Y/m/d H:i` timestamps -> `<ts>`; `H:i` times -> `<time>`;
32/40/64-hex -> `<hex32>/<sha1>/<hex64>`; `set-cookie` header values
normalized the same way.

## Reuse for the Rust server

1. `docker compose down --volumes && docker compose up -d`
2. `oracle/scripts/restore.sh` (restores golden-db.sql; skip re-seed)
3. Keep the updater stopped; run the same scenario against the Rust base URL
   and diff against `oracle/data/runs/oracle-a` (or a fresh oracle run).
