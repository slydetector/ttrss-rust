# Oracle: dockerized PHP tt-rss + golden dataset

> The **record/replay harness is retired (2026-09-19) and removed (2026-10-27)**:
> the corpus, the `crates/harness` diff tool and the `make harness`/`record`
> targets are gone. Verification is the Rust test suite (unit/DB tests +
> PHP test-parity; see `AGENTS.md`). What remains is the live stack, the
> golden dataset and the param recorders.

The oracle is a live, stock Tiny Tiny RSS (PHP) deployment:

1. **Reference implementation** for the ttrss port — run stock PHP side by
   side with the Rust server for parity checking.
2. **Development database** — `make run` / `make demo-*` point the Rust
   server at the oracle Postgres (golden dataset, `oracle/.env` creds).
3. **Param-differential recorders** — `make record-params` drives the real
   PHP helpers and records the fixtures the Rust tests compare against
   (no oracle stack required, just the public PHP image).
4. **Performance baseline**: `scripts/bench.sh` measures PHP-FPM latency for
   the key endpoints.

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
│   ├── check_sync.py     # verify recorded dataset matches the current seed recipe
│   ├── record_params.sh  # re-record the param-differential fixtures (PHP 8.5 container)
│   ├── bench.sh          # latency baseline -> oracle/data/baseline.json
│   └── census.sh         # nginx request-distribution summary
└── data/                 # generated (gitignored)
    ├── golden-db.sql     # frozen full DB dump (the golden dataset)
    ├── cache/            # copied app cache (feed icons, cached feed XML)
    ├── db-manifest.json  # row counts + key facts
    ├── ids.json          # feed/category/user ids + credentials
    └── baseline.json     # PHP-FPM latency baseline
```

## Stack

- `db` — postgres:17-alpine, single database `postgres`.
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
- 4 users:
  - `admin/admin` — admin; **no TOTP** (the frictionless demo login path);
    **app password** `AppPass123` for the API (service `_api`);
    app-password + API-access prefs set.
  - `otpuser/otppass` — regular user with **TOTP enabled** (secret
    `OTPSECRET1234567890`, 6-digit/30s/SHA-1; the column stores the ASCII
    secret, the effective HMAC key is its ASCII bytes) — demonstrates the
    OTP challenge + code flow without putting it on the admin demo path.
  - `readonly/readpass` — read-only (no feeds of its own; useful for
    not-found / permission edge cases).
  - `testuser/testpass` — created via `update.php --user-add` (CLI path).
- 2 profiles (Home/Work), 2 access keys (feed `a1b2c3d4e5f6g`,
  category `g6d4c3b2a1e8f`), error-log entries from the broken feeds.
- Fixed `last_login` on all users so the users-admin page is stable across
  corpus runs.

The seed stops the updater daemon before recording feed state, so the
snapshot is frozen (no background feed updates during corpus runs).

### Sync gate (seed recipe ↔ recorded dataset)

`db-manifest.json` records a `lineage` block: the SHA-256 of the
`seed.py` that produced the recorded dataset, and the SHA-256 of
golden-db.sql. `check_sync.py` verifies that the working-tree seed
recipe still matches that lineage, and (when the stack is up) that live
row counts match the manifest:

```sh
make oracle-check                # or: python3 oracle/scripts/check_sync.py
```

If you edit `seed.py`/`feeds.txt` (or patch the DB out-of-band), the gate
fails until you re-baseline. Pick one:

- `make oracle-bless` — cheap: re-records the lineage hashes from the files
  on disk (no re-seed, no live fetch, no docker). Use it when the golden
  dump is still the baseline you want (e.g. a comment-only recipe edit).
- `make oracle-seed && make oracle-snapshot` — regenerate the dump from the
  recipe (re-fetches live feeds, ~minutes; a fresh stack needs `oracle-up`
  first). Use it when the recipe's *data* actually changed.

The recorded dataset is only valid for the seed recipe that produced it.

## Reuse for the Rust server

**The canonical interface is the top-level `Makefile`** — the `oracle-*`
targets manage the stack, `make run` / `make demo-*` run the Rust server
against the oracle database (golden baseline via `make demo-reset`):

```sh
make oracle-up / oracle-down     # manage the PHP stack
make oracle-seed                 # regenerate the golden dataset
make oracle-snapshot / -restore / -reset / -check
make run                         # run the Rust server (foreground, :9000)
make record-params               # re-record param fixtures from frozen PHP
```
