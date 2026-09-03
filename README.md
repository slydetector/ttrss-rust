# tt-rust

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

## Quick start (oracle)

```sh
oracle/scripts/up.sh          # bring up the PHP oracle (db + app + updater + nginx)
curl http://127.0.0.1:8280/tt-rss/public.php?op=healthcheck   # -> OK
oracle/scripts/down.sh        # tear down (keeps the db volume)
```

Oracle login: `admin` / `admin` (TOTP secret `JBSWY3DPEHPK3PXP`; API app
password `AppPass123`). See `oracle/README.md` for the full oracle guide.

## Harness (record / replay / diff)

```sh
cargo build --bin tt-rust-harness

# record the 58-step corpus against the oracle
target/debug/tt-rust-harness run \
    --scenarios oracle/corpus/scenarios.template.toml \
    --vars oracle/data/ids.json \
    --out oracle/data/runs/<name>

# diff two runs (self-replay must be clean; Rust runs diff vs an oracle run)
target/debug/tt-rust-harness diff \
    --a oracle/data/runs/oracle-a --b oracle/data/runs/oracle-b
```

## Status

- **M0 complete**: oracle + golden dataset + 58-step corpus; harness proven
  with a clean 58/58 self-replay diff; PHP-FPM baseline in
  `oracle/data/baseline.json`. See `docs/plan.md` (M0 status section).
- Next: **M1 walking skeleton** — `tt-rust serve` (static assets + login page
  + real login + SPA shell + DB sessions).
