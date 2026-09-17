# Oracle: dockerized PHP tt-rss + golden dataset + corpus

The oracle is a live, stock Tiny Tiny RSS (PHP) deployment that serves two
purposes:

1. **Reference implementation** for the ttrss port: the harness records its
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
│   ├── check_sync.py     # verify recorded dataset matches the current seed recipe
│   ├── record_params.sh  # re-record the param-differential fixtures (PHP 8.5 container)
│   ├── bench.sh          # latency baseline -> oracle/data/baseline.json
│   └── census.sh         # nginx request-distribution summary
├── corpus/
│   └── scenarios.template.toml   # the 61-step record/replay suite (53 active; 8 skipped)
└── data/                 # generated (gitignored)
    ├── golden-db.sql     # frozen full DB dump (the golden dataset)
    ├── cache/            # copied app cache (feed icons, cached feed XML)
    ├── db-manifest.json  # row counts + key facts
    ├── ids.json          # feed/category/user ids + credentials for the harness
    ├── baseline.json     # PHP-FPM latency baseline
    └── runs/<name>/      # harness fixtures: NNN_<step>.json
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
`harness.sh` runs the same check before build/reset, so a drifted dataset
can never silently serve as the baseline of a harness run.

## Corpus

`corpus/scenarios.template.toml` defines 61 ordered steps covering (steps
marked `skip = true` are kept for re-enablement but not executed or diffed)

- public.php: healthz, login page, static assets
- web login (admin: frictionless username/password → 302), shell, prefs
  shell, ETag/304
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
- logout; readonly user login + feed tree + headlines ("Feed not found");
  otpuser login (OTP challenge 200 → code 302), shell, logout.
  Skipped: checkforupdates (011, live GitHub data), digest preview (032,
  time-window drift), the admin logout flow (051–052), and the otpuser
  flow (058–061) — see `docs/plan.md` (harness triage).

Run it:

```sh
harness run --scenarios oracle/corpus/scenarios.template.toml \
    --vars oracle/data/ids.json --out oracle/data/runs/<name>
```

Variables like `{{totp:{{totp_secret}}}}` compute live TOTP codes. Steps
re-execute per run, so volatile session/CSRF/API-sid values are always
re-derived; `--vars` supplies the stable seeded facts.

## Proving the harness (self-replay)

Two runs minutes apart must diff clean after normalization:

```sh
harness run ... --out oracle/data/runs/oracle-a
harness run ... --out oracle/data/runs/oracle-b
harness diff --a oracle/data/runs/oracle-a --b oracle/data/runs/oracle-b
# -> 61 exchange(s): 61 match, 0 mismatch
```

Normalizations applied at diff time: CSRF tokens / session ids / sids dropped
from JSON; `recent_log_events` dropped (grows with request history);
ISO/RFC822/`Y/m/d H:i` timestamps -> `<ts>`; tt-rss "smart dates" (`13:28`,
`1 Jan, 12:00`, `Thu, 1 Jan 2026` — display renderings of stored datetimes
that logins/updates re-stamp between runs) -> `<ts>`;
32/40/64-hex -> `<hex32>/<sha1>/<hex64>`; URL authority ports stripped
(`http://127.0.0.1:8280/...` == `http://127.0.0.1:8281/...`, literal and
percent-encoded) so the oracle (8280) and Rust (8281) compare; all header
values (notably `set-cookie` and `location`) normalized the same way.

## Reuse for the Rust server

**The canonical interface is the top-level `Makefile`** — `make harness` resets
the oracle DB to the golden baseline (reproducible), builds the server + harness,
starts the Rust server on `127.0.0.1:8281` (self URL `http://127.0.0.1:8281/tt-rss/`),
runs the corpus, and diffs against `oracle/data/runs/oracle-a` (it calls
`oracle/scripts/harness.sh`):

```sh
make harness                         # full run + diff vs oracle-a
make harness ARGS=--no-build         # reuse existing binaries
make harness ARGS=--no-reset         # skip the DB reset
make harness-keep                    # leave the Rust server running
make run                             # just run the Rust server (foreground)
make record-params                   # re-record param fixtures from frozen PHP
```

The diff strips the authority port, so the oracle (8280) and Rust (8281) compare
as the same application on different ports.

> **The harness is used for read-only ops for now.** The corpus is *stateful*
> (logins create/destroy sessions, `Feeds::view` persists `_DEFAULT_VIEW_*`,
> the readonly/OTP logins at the end mutate user state). That's why `harness.sh`
> resets the oracle DB to the golden baseline before every run — without the
> reset, mutations accumulate across runs and the diff drifts (steps stop being
> written, counters/orderings shift). The write/mutating steps are diffed only
> against a matching-baseline oracle run and will get dedicated write-op
> verification (asserting the *resulting* DB state, not just the response) at a
> later milestone. The DB reset keeps each run reproducible regardless.
