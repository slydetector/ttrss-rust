# ttrss

Rust re-implementation of the [tt-rss](https://github.com/tt-rss/tt-rss) (Tiny Tiny RSS)
server backend. The existing PHP codebase in `tt-rss/` is the frozen reference
implementation and specification; the Dojo/JS frontend is served unchanged.

See `docs/plan.md` (delivery plan), `docs/research-report.md` (methodology), and
`docs/test-coverage-audit.md` (reference test baseline).

## Layout

| Path | Contents |
|---|---|
| `crates/` | Rust workspace crates (`harness` = record/replay/diff test harness) |
| `oracle/` | Dockerized live PHP tt-rss instance used as the test oracle; scripts to seed the golden dataset, snapshot it, and drive the corpus |
| `oracle/data/` | Generated: golden DB dump, corpus fixtures, census, perf baseline (gitignored) |
| `docs/` | Planning & research documents |
| `tt-rss/` | Upstream PHP checkout (frozen reference, own git repo) |

## Prerequisites

- Rust (stable), Docker + compose, Python 3 (seed script)

## Quick start

The top-level **`Makefile`** is the primary interface — `make` (or `make help`)
lists every target. The most common targets:

```sh
make oracle-up      # bring up the PHP oracle (db + app + updater + nginx)
make oracle-down    # tear down (keeps the db volume)
make build          # build the Rust server + harness
make run            # run the Rust server in the foreground (port 8281)
make harness        # reset DB + build + run the corpus + diff vs oracle-a
make test           # run the full workspace test suite
```

The oracle (PHP) web is on `http://127.0.0.1:8280/tt-rss/`; the Rust server on
`http://127.0.0.1:8281/tt-rss/` (the diff strips the port, so they compare as
the same app). Oracle login: `admin` / `admin` (no TOTP — the frictionless demo
path; API app password `AppPass123`); `otpuser` / `otppass` (TOTP enabled,
secret `OTPSECRET1234567890`). **`oracle/README.md` is the full oracle + harness
guide — read it before driving the corpus.**

## Harness (record / replay / diff)

`make harness` is the canonical run (resets the oracle DB to the golden
baseline so runs are reproducible, builds, starts the Rust server on `:8281`, runs
the 53-step corpus (61 defined; 8 `skip = true`), and diffs against `oracle/data/runs/oracle-a`):

```sh
make harness                        # full run + diff vs oracle-a
make harness ARGS=--no-build        # reuse existing binaries
make harness-keep                   # leave the Rust server running
```

To *record* a fresh oracle baseline, or to diff two run dirs by hand:

```sh
make record                         # record -> oracle/data/runs/oracle-a
make diff ARGS=--a <run-a> --b <run-b>
```

## Status

- **M0 complete**: oracle + golden dataset + corpus; harness proven with a
  clean self-replay diff; PHP-FPM baseline in `oracle/data/baseline.json`.
- **M1 complete**: `ttrss serve` (static assets byte-for-byte + login page +
  real login SSHA-512 + TOTP + logout + authenticated SPA shell with
  ETag/304 + encrypted DB sessions with rolling cookie re-stamp). The full
  login/logout/session surface diffs clean against the oracle; the corpus is
  now 61 steps (admin is frictionless; a dedicated `otpuser` exercises the
  OTP challenge + code path). See `docs/plan.md` (M1 status section).
- Next: **M2 read-only UI on real data** — the `backend.php` RPC surface
  (`getFeedTree`, `Feeds.view`, …) + external API read ops so the feed tree
  and headlines render on the golden dataset.
