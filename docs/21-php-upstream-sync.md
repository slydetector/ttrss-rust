# Syncing with PHP upstream (the tt-rss php submodule)

The `tt-rss` php submodule is the spec, pinned by the superproject.
Upstream is actively maintained (Weblate translation churn, Dependabot
bumps, features, occasional security advisories), so the pin deliberately
moves. A pin bump **changes the spec** — the acceptance bar for every sync
is the full Rust test suite + CI e2e green **against the new pin**.

## How the port couples to the php upstream

**Auto-propagated by a pin bump — no port work** (all read from the docroot
at runtime, so the php file is the single source of truth in every case):

| php path | how the port consumes it | evidence |
|---|---|---|
| `js/`, `themes/`, `images/` | served as static files from the docroot | `crates/ttrss/src/render.rs` |
| `locale/<loc>/LC_MESSAGES/messages.mo` | lazy-loaded per locale at runtime | `crates/ttrss/src/i18n.rs` |
| `sql/pgsql/schema.sql` | executed at fresh-install; also the test fixture (since `6a6777b`) | `core/schema.rs`; `test-db` |
| `sql/pgsql/<v>.sql` | applied by `migrate_to` straight from the docroot | `core/schema.rs::migrate_to` |

**Needs porting work:**

- `classes/*.php` — behavior already ported to Rust (RPC dispatch, sessions,
  feed logic). Any change is a spec change for the port.
- a **new numbered migration** (`sql/pgsql/<v>.sql`) — the port applies the
  upstream file unchanged, but three things must move with it:
  `Config::SCHEMA_VERSION` (the max the build knows), the fixture stamp in
  `crates/test-db/fixtures/baseline.sql` (it hardcodes the version a fresh
  install records), and the matching assertion in `crates/test-db/tests/
  smoke.rs`.
- `update.php` CLI semantics changes (the M8 CLI tail tracks it).
- `include/*` (DB-layer/common semantics — rare).
- `.docker/` — **considered, not ported**: upstream's official image
  definition (base images, env defaults, nginx config). Changes can affect
  the port's drop-in image compatibility (docs/19), so they surface as
  REVIEW items to evaluate, not to port.

**Ignored:** `.github/`, `package*.json`, upstream docs —
upstream's own infrastructure.

## Classify by *commit*, not by file

A commit that touches `js/` looks auto — until it's the frontend half of a
backend feature: upstream #449 (filter entries by feed title) changed
`js/Headlines.js` *and* `classes/Feeds.php` in one commit. Auto-propagating
just the JS would ship a UI control calling behavior the port doesn't have.
So classification is per commit:

- **REVIEW** — the commit touches anything under `classes/`, `update.php`,
  `include/`, `sql/`, or `.docker/` → port it with test parity (upstream's
  new/changed PHP test cases land as Rust tests, per the standing rule);
  `.docker/` items are evaluated, not ported.
- **AUTO** — everything else (`locale/`, `js/`, `themes/`, `images/`,
  `messages.pot`, upstream infra) → note it; the pin bump propagates it.

Policy: **port every REVIEW item — no cherry-picking.** Fixes, security, and
features all land; the port tracks the spec, not a curated subset.

## The tool: `make php-sync`

```
make php-sync    # runs scripts/php-sync.sh: fetches tt-rss origin,
                 # reports pin..origin/main
```

Prints the range summary, then:

- **REVIEW section** — each qualifying commit with its **full diff inline**
  (the artifact to port from; no separate diffing step).
- **AUTO section** — one line per commit.

Notes: merge commits are skipped (`--no-merges`; their content is subsumed
by the non-merge commits in the range). Classification is by path prefix —
if upstream ever adds a new top-level backend directory, extend the regex.

## Workflow

1. `make php-sync` — read the report.
2. Port each REVIEW commit in order. For new user-facing strings, copy the
   **exact msgid** from the PHP so the existing `.mo` catalogs apply.
3. If the report shows a new `sql/pgsql/<v>.sql`: bump
   `Config::SCHEMA_VERSION`, the fixture stamp in `baseline.sql`, and the
   smoke-test assertion (all three hardcode the version); the DB-backed
   tests exercise the migration path.
4. Bump the pin: `git -C tt-rss checkout <new-sha>`, then commit the
   superproject pointer with the port changes:
   `php-sync: bump tt-rss <old7>..<new7>, port <upstream shas>`.
5. `cargo test` (full suite) locally; CI e2e on push. Green = synced.

## Cadence

On demand — the report is a fetch plus a diff, seconds to run. Do it before
shipping the Docker image, and immediately when an upstream security
advisory (GHSA) lands — advisories always touch `classes/`, so they surface
as REVIEW items.

## Backlog at time of writing (2026-10-10: pin `2f7d831` → upstream `270fcc7`)

4 REVIEW commits:

- `bdb1c47` — validate the session in `dbupdate` (**GHSA-9f8c-5cv6-jv5x**,
  security)
- `e88133e` — mark session invalid on access-level changes (security)
- `9a6b861` — filter entries by feed title (#449, feature: a `feed` keyword
  filter in entry queries)
- `820aeae` — base-images-bump (#444, `.docker/` — evaluate, not port)

16 AUTO commits (Weblate translations, npm/DevDep bumps, upstream GH
workflow bumps, plus `5f8cd06` — JS-only switch to idempotent mark/publish
RPC calls; the port already dispatches the individual mark/unmark/publish
actions, so no work). No `sql/` changes → no `SCHEMA_VERSION` bump needed.
