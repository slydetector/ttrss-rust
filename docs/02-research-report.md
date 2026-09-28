# Research Report: Best Practices for Large Cross-Language Ports + ttrss Methodology

Purpose: inform the tt-rss → Rust backend port process. Covers language-agnostic
practices for large rewrites, then applies them to this project: oracle strategy,
revised milestone plan, CLI/daemon recommendations, and performance strategy.

---

## Part 1 — What the research says

### 1.1 Incremental replacement beats big-bang rewrites

- The **Strangler Fig pattern** (originally Fowler; widely documented, e.g.
  Microsoft Azure Architecture Center<sup>1</sup>, Laminas<sup>2</sup>) is the
  standard answer to "replace a system": build the new system around the old one
  and replace functionality piece by piece, keeping the old system running until
  the last piece lands. The consistent advice across sources: **never let the new
  system be a big-bang**; each increment must be demonstrably working.
- Agile experience reports converge on the same mechanics: start with a
  **"walking skeleton"** — a minimal end-to-end slice that exercises every layer
  (deployment, auth, DB, UI) with stubs, then thicken it slice by slice
  ("vertical slices" / "steel thread")<sup>3</sup><sup>4</sup>. This directly
  matches the user requirement of *demoable checkpoints from simple to complex*.
- Practical rewrites of huge codebases (e.g. DZone's "How To Rewrite a Huge
  Codebase"<sup>5</sup>) use **contract-first** approaches: extract a formal API
  contract (OpenAPI etc.) + the existing test suite, generate server stubs in the
  target language, and fill them in until *all existing tests pass*. The
  executable test suite is the spec, not prose documentation.
- SE/StackExchange practitioner consensus: a faithful 1:1 port is ~an order of
  magnitude *slower than it looks*, because the bottleneck is **understanding
  behavior**, not writing code<sup>6</sup>. Budget for discovery, not just
  translation.

**Implication for ttrss:** no phase may exist that "only makes sense in the
future." Every milestone must run end-to-end (serve, log in, show data). The
frontend stays frozen (it is the spec and the UI); the server is replaced in
slices behind the same HTTP surface.

### 1.2 The "feature parity trap" (Martin Fowler)

Fowler's *Patterns for Displacing Legacy Systems → Feature Parity*<sup>7</sup>
is the most on-point source found:

- Teams **greatly underestimate** the effort of "do exactly what the existing
  system does"; even *defining* the "as-is" scope is itself a huge effort.
- Legacy systems bloat: a large fraction of features is unused (Standish 2014:
  ~50%), and some "limitations" become load-bearing business behavior.
- To do parity well you must produce a **specification of current behavior** from:
  - **System surveys** — user roles, every action, batch jobs, interfaces
    provided/consumed, core algorithms, reports, and *data mutations* (triggers,
    stored procedures — for tt-rss: DB-side behavior, counters, migrations).
  - **Archeology** — discovering hidden couplings by experiment ("change field Y
    on screen A, value Z appears in report C after batch N").
  - **Instrumentation** — *measure what is actually used* (access logs, request
    logs) so you can prioritize by real usage instead of by code size.
  - **Feature value** — explicitly dropping low-value features to cut scope.
- Parity is *proven with tests*, in layers:
  - **Acceptance/user-journey tests** (top of pyramid; don't rely on them alone).
  - **Executable contracts**: build a test suite that runs against the *old*
    system and then against the *new* one (the old system is the oracle).
  - **Record/replay via proxies**: inject a proxy in the communication path to
    record interactions; replay against the new system and compare
    (explicitly recommended in the article).
  - **Port the unit tests** of the old system into the new stack as executable
    specs; consider **mutation testing** for extra confidence.
  - Databases/reports are "another interface" and need their own parity tests.

**Implication for ttrss:** the PHP codebase + a running PHP instance *is* the
spec. We need (a) a recorded executable contract for every HTTP surface
(`backend.php`, `api/`, `public.php`, shells), (b) instrumentation of the oracle
to see which ops the UI actually exercises, and (c) a scope decision per feature
(port / simplify / drop) rather than blanket 100%.

### 1.3 Golden-master / characterization testing

- **Characterization (golden-master) testing** was coined by Michael Feathers
  (*Working Effectively with Legacy Code*): "legacy code = code without tests";
  you first *capture actual behavior* as tests, then change<sup>8</sup><sup>9</sup>.
  The purpose is "to document the system's actual behavior, not the behavior you
  wish it had"<sup>9</sup>.
- The mechanics: fix the inputs (fixture data + request corpus), record the
  actual outputs (snapshots), freeze them as the expected values, and re-run
  after the change. For HTTP services this is exactly **record-and-replay**:
  record responses from the live legacy service, normalize non-determinism
  (timestamps, ids, sequences), store as fixtures, assert the new service
  matches.
- Tooling precedent: **GoReplay** (capture/replay live HTTP traffic)<sup>10</sup>,
  and **Twitter's Diffy**: run the same (or replayed) traffic against two
  implementations and compare responses; "if two implementations return similar
  responses for a sufficiently large and diverse set of requests, they can be
  treated as equivalent and the new one is regression-free"<sup>11</sup><sup>12</sup>.
  Diffy is explicitly designed to tolerate/normalize "noise" differences.

**Implication for ttrss:** build the golden-master harness *first* (before
porting logic), driven by a dockerized PHP oracle: (1) a **populated reference
DB** (pg_dump) as the data fixture, (2) a **request corpus** (scripted browser
session + API calls) with recorded responses as fixtures, (3) a **diff tool**
that normalizes known non-determinism (timestamps, CSRF tokens, session ids,
cache-bust query strings, `last_updated` etc.) and reports structural/semantic
diffs. This harness is the acceptance gate for every milestone.

### 1.4 Data & cutover strategy

- Standard practice for replacing systems with existing data: **shared/compatible
  data model + clean cutover**, optionally with a **parallel-run window** where
  the legacy system stays up (read-only) for 24–72 h for rollback
  <sup>13</sup><sup>14</sup>. AWS prescriptive guidance treats cutover as a
  discrete, rehearsed event with rollback criteria<sup>15</sup>.
- For this project the user chose *clean cutover, no cohabitation*, with the
  requirement that the Rust server must demonstrably work on a **fully populated,
  PHP-originated dataset**. That is exactly the golden-dump + differential-test
  setup above: no dual-write machinery needed, just (a) reuse the exact schema +
  migration files so any PHP-produced DB is valid input, and (b) test against a
  realistic dump rather than synthetic fixtures.

### 1.5 Decomposition guidance (language-agnostic)

Synthesizing the sources, the recurring recommendations for breaking a large port
into safe pieces:

1. **Boundaries before interiors.** Port the interfaces (HTTP API shapes, DB
   schema, file formats, CLI flags) first; they are the cheap, high-signal
   contracts. The interiors (business logic) are verified *against* the
   boundaries.
2. **Data layer first, then read paths, then write paths, then background jobs,
   then extensions.** Read-only is the easiest to verify and the fastest to a
   working demo; writes mutate the golden data and need the oracle to
   re-verify state; background jobs (daemon, digest, scheduler) are the least
   visible and can come later.
3. **One demoable increment at a time.** Each increment: small vertical slice,
   end-to-end runnable, verified against the oracle, demonstrable to a human.
4. **Keep the old system running as the oracle throughout** (don't retire it
   until the final milestone) — including re-running it to *regenerate* fixtures
   when you discover a gap.
5. **Instrument to deprioritize.** Use the oracle's access/error logs to find
   what the UI actually calls; port hot paths first, treat cold paths (e.g.
   plugin installer) as defer-able.
6. **Non-determinism is the enemy of diffing.** Normalize deliberately
   (clocks → fixed/fake time where possible, ids/sequences → pattern-match,
   random tokens → structural check).
7. **Verify the verifiers.** Golden fixtures can bake in bugs; spot-check
   fixtures against the *documented* intent (API docs, UI behavior) and against
   the existing PHPUnit suite, which encodes known-correct behavior.
8. **Mutation/edge testing for safety-critical logic** (sanitizer, SSRF guards,
   auth, crypto) beyond what golden files cover — fuzz + property tests.

---

## Part 2 — Applied to ttrss

### 2.1 Decisions locked in (from user answers)

| # | Decision |
|---|---|
| 1 | Frontend (Dojo/JS) is frozen; Rust serves it byte-for-byte. |
| 2 | Native Rust plugin API + ported first-party plugins; community PHP plugins unsupported; **git-based plugin installer deferred** (noted for later; UI shows listing only). |
| 3 | PostgreSQL, clean cutover (no PHP/Rust cohabitation), but Rust must pass tests on a fully populated PHP-originated dataset. |
| 4 | External JSON API (level 23) compatibility maintained. |
| 5 | CLI/daemon: keep the existing surface unless clearly better — any changes must be proposed *before* implementation (see §2.5). |
| 6 | MVP-first with demoable end-to-end checkpoints; confidence marker = **web UI comes up in read-only mode on real data**. |
| 7 | Oracle: dockerized live PHP instance (researched below; the docker setup lives in the main repo's `.docker/` + published images, not a separate repo). |
| 8 | Project name: **ttrss**. |
| 9 | Performance goal: Rust must be *faster* than PHP-FPM (no hard targets). |

### 2.2 The oracle (dockerized PHP tt-rss)

Research findings (sources: tt-rss Installation Guide<sup>16</sup>, repo
`.docker/`, Docker Hub<sup>17</sup>):

- Docker setup is **inside the main repo**: `.docker/app/Dockerfile` (Alpine +
  PHP 8.5-FPM, PostgreSQL client, pcntl) and `.docker/web-nginx/Dockerfile`
  (nginx front), plus `startup.sh`/`updater.sh`/`config.docker.php`. The user's
  "separate repo" recollection matches the *published images* and the
  `tt-rss-web-nginx` repo referenced by Docker Hub; functionally we can either
  **pull** the published images or **build** from the repo checkout (both work;
  building from the exact commit is preferable so the oracle matches the code we
  read).
- Published images (multi-arch, built per commit by GitHub Actions):
  - Docker Hub: `supahgreg/tt-rss`, `supahgreg/tt-rss-web-nginx`
  - GHCR: `ghcr.io/tt-rss/tt-rss`, `ghcr.io/tt-rss/tt-rss-web-nginx`
  - Tags include `latest` and per-commit `sha-<shortsha>` tags → **we should pin
    to the `sha-` tag matching our source checkout** for a reproducible oracle.
- Topology (from `docker-compose.yml` in the repo): `postgres:15-alpine` +
  `app` (FPM) + `updater` (multiprocess daemon) + `web-nginx` (port 8280 →
  `http://127.0.0.1:8280/tt-rss`, login `admin` / `ADMIN_USER_PASS`).
- Key env knobs (`.env-dist`): `ADMIN_USER_PASS`, `ADMIN_USER_ACCESS_LEVEL`,
  `AUTO_CREATE_USER{,_PASS,_ACCESS_LEVEL}`, `TTRSS_DB_{USER,NAME,PASS,HOST,PORT}`,
  `HTTP_PORT`, plus any `TTRSS_*` config override (e.g. `TTRSS_PLUGINS`,
  `TTRSS_ENCRYPTION_KEY`).

**Oracle plan (milestone M0):**

1. `oracle/` dir in ttrss: pinned `docker-compose.yml` + `.env` mirroring the
   repo's, images pinned to the checkout's `sha-` tag (fallback: build local
   images from `.docker/`).
2. Bring up: db + app + updater + nginx. Verify `healthcheck` and login.
3. **Populate a reference dataset** (scripted, reproducible via a `seed` script):
   - admin (level 10) + a regular user (level 0) + a readonly user (level -1);
   - ~40–60 real RSS/Atom feeds (mix: standard RSS 2.0, Atom, RDF, namespaced,
     weird encodings, feeds with enclosures/media, one slow feed, one 429 feed,
     one failing feed) across nested categories + labels;
   - run the updater for a while (or `update.php --feeds` a few times) to fill
     `ttrss_entries`/`ttrss_user_entries`, counters, icons, enclosures;
   - exercise: filters (rules+actions), article notes/scores/tags, starred/
     published, OPML import, profiles (2nd profile), app password, TOTP enabled
     on one user, `ENCRYPTION_KEY` set (encrypted sessions + feed passwords),
     several first-party plugins enabled per-user, custom theme + CSS.
4. **Golden artifacts**:
   - `pg_dump` of the populated DB → `oracle/data/golden-db.sql` (the dataset
     fixture for the Rust test suite);
   - **request corpus**: a scripted driver (Selenium or plain HTTP client walking
     every `backend.php`/`api`/`public.php` op the JS uses) that executes a
     defined scenario list and records (method, URL, headers, body, status,
     response) pairs → JSON fixtures;
   - **usage census**: parse the oracle's access log + `ttrss_error_log` after
     the scripted UI session to rank ops by real usage (the instrumentation
     Fowler recommends) — this drives porting order and the drop/simplify list.
5. Keep the oracle stack re-runnable from CI (compose + seeded dump) so any
   milestone can diff against a fresh PHP run, and regenerate fixtures when
   gaps are found.

### 2.3 Revised delivery plan (milestone-first, simple → complex)

Replaces the earlier phase list. Rule: **each milestone ends with a live,
demoable end-to-end system** and a green oracle-diff harness for everything it
claims. (Detailed per-milestone work items live in 01-plan.md §3, which will be
updated to this ladder.)

| M | Milestone | Demo / exit criteria |
|---|---|---|
| **M0** | Oracle + harness | Dockerized PHP oracle up with populated golden DB; request-corpus recorder; diff tool with normalizers; CI runs "record vs replay" against a *stub* Rust server to prove the harness works |
| **M1** | Walking skeleton | `ttrss serve`: serves all static frontend assets + login page; real login (`auth_internal`, SSHA-512 verify) against golden DB; SPA shell renders in a real browser (read-only, empty state OK); sessions in DB (encrypted) |
| **M2** | **Read-only UI on real data** (the confidence marker) | Feed tree + categories + counters + headlines + article content + search + labels all correct on the golden dataset; `backend.php` read ops + `api/index.php` read ops (level 23) pass the diff harness; Atom/JSON syndication + feed icons + ETag shells |
| **M3** | Mutations I: article state | mark/unread/publish/star, catchup (feed/cat/all), notes, scores, labels assign/remove, tags; diff harness asserts both *responses* and *resulting DB state* vs PHP |
| **M4** | Mutations II: configuration | prefs (toggle/set, profiles), feed management (subscribe/edit/remove, categories, icons, ordering, OPML import/export), filter CRUD + execution on update, labels CRUD; access-key/syndication management |
| **M5** | External API completeness + admin | Full level-23 API incl. `updateFeed`/`subscribeToFeed`/`shareToPublished`; `Pref_Users`, `Pref_System` (log, test email), password recovery (forgotpass), OTP/QR |
| **M6** | The engine: updates | `ttrss update` CLI + daemon (tokio workers): fetch/parse/hash/dedup/filters/enclosures/favicons, counter cache maintenance, failed-feed disabling, scheduler/cron housekeeping, digest emails; run the updater on the golden dataset and diff resulting DB state vs PHP updater run |
| **M7** | Plugin system + first-party plugins | Rust plugin API; port auth_internal, auth_remote, nsfw, note, af_* (incl. psql_trgm), share, bookmarklets, cache_starred_images, hotkeys; plugin storage + per-user enablement portable |
| **M8** | CLI/daemon parity + polish | All `update.php` subcommands (users, OPML, gen-*, debug-feed, cleanup-tags…), lockfile semantics, `--update-schema` (reuse exact `.sql` migrations) |
| **M9** | Hardening & performance | Fuzz (sanitizer/parser/URL), security matrix, perf vs PHP baseline on golden dataset (target: faster), Docker image + docs + cutover runbook |

Notes:
- M2 is deliberately the user's confidence marker and comes *before* any writes
  — read paths are cheapest to verify and immediately demoable.
- The plugin installer (git-based) stays deferred: M7 ships read-only plugin
  listing (`getPluginsList`/`getAvailablePlugins` backed by bundled plugin
  metadata); install/update UI is disabled or stubbed until a later decision.
- Email (lettre) appears in M6 (digests) and M5 (test email / password reset) —
  both are thin over one mailer module.

### 2.4 Test strategy (per research §1.2–1.5)

1. **Golden-master diff harness** (the spine): fixtures from M0; every
   milestone must not regress it; normalize timestamps/CSRF/session ids/
   cache-busters/ETags; compare JSON *structurally* (type-coerced per the
   loose-type rules), HTML shells *structurally*, Atom *structurally*.
2. **DB-state differential**: for write milestones, run the same op on the PHP
   oracle and the Rust server against identical DB snapshots, then diff the
   relevant tables.
3. **Executed contract**: port `tests/integration/ApiTest.php` + key
   `tests_functional` suites as Rust integration tests against the live oracle
   fixtures.
4. **Unit + property tests** for the risky leaves: type coercion, crypto
   interop vectors (PHP-generated hashes/sessions), TOTP, cron, time formatting.
5. **Fuzz** for sanitizer / feed parser / URL validator (cargo-fuzz), plus the
   nasty-HTML corpus from the PHP SanitizerTest.
6. **Selenium smoke** (existing `selenium_test.py`) at M2 and each milestone.
7. **Perf gate**: timed headline-query / feed-tree / update-run benchmarks from
   M2 onward; PHP numbers recorded as the baseline.

### 2.5 CLI/daemon: proposed approach (to confirm before implementation)

Per decision #5, here is the thinking, *before* acting:

**Keep (for ops compatibility):**
- Exact subcommand names and flags of `update.php` (`--feeds`, `--daemon`,
  `--daemon-loop`, `--update-feed:N`, `--user-add:…`, `--opml-export:…`,
  `--update-schema[::force-yes]`, `--gen-encryption-key`, `--gen-search-idx`,
  `--plugins-list`, `--debug-feed:N`, `--force-update`, `--force-refetch`,
  `--force-rehash`, `--cleanup-tags`, `--send-digests`, `--log`, `--log-level`,
  `--quiet`, `--task:N`, `--pidlock:N`, `--force-yes`, `--help`) with the same
  `opt:value` getopt style where scripts may depend on it.
- Lockfile names and locations (`lock/update_daemon.lock`,
  `lock/update_daemon-<pid>.lock`, `update.lock`, stampfile), spawn-interval
  floor (60 s), per-task stagger (`task*5` s), child runtime cap
  (`DAEMON_MAX_CHILD_RUNTIME`).
- Exit codes used by scripts (0 ok, 1 generic failure, 100/110 update-feed
  failure classes).

**Proposed (genuine improvements, flagged for your approval):**
1. **One binary `ttrss` with subcommands** instead of three entry scripts:
   `ttrss serve` (web), `ttrss update …` (CLI ops, accepts legacy
   `--opt:value` flags), `ttrss daemon …` (long-running updater; also usable
   as `ttrss update --daemon` for exact PHP parity). Rationale: one artifact
   to ship, shared core; flag surface unchanged.
2. **No forking.** Workers are tokio tasks in one process (PHP needed forks for
   process isolation + `pcntl`; Rust gets concurrency natively). Lockfiles are
   kept for external tooling and single-instance semantics; SIGTERM does a
   clean shutdown of in-flight updates.
3. **Structured logging** (`tracing`, JSON option) while keeping the
   `--log file` / `--log-level 0-2` / `--quiet` contract.
4. **`serve` + `daemon` can run in one container or two** (both layouts
   supported; default compose mirrors today's: one `serve`, one `daemon`).

If you'd rather keep three separate entrypoints (`ttrust-web` / `ttrust-update`
/ `ttrust-daemon`) for 1:1 operational mapping, that's a one-line packaging
change; everything else above stands.

### 2.6 Performance strategy ("faster simply by porting")

- **Baseline first (M0/M2)**: record PHP timings on the golden dataset — feed
  tree render, headline page (200 items w/ content), global counters,
  search query, per-feed update time — with the same Postgres, same machine
  class. These are the bars.
- **Keep tt-rss's own optimizations** (they're in the SQL): counter caches
  (`ttrss_counters_cache`/`ttrss_cat_counters_cache`), `int_id` user-entry
  cache, `tsvector` search index, conditional feed updates.
- **Structural wins the Rust port gets for free**: async/parallel feed
  fetching in the daemon (reqwest) vs PHP's fork-per-batch model; no per-request
  process cost (FPM workers); batched SQL (PHP issues a counter query per feed
  in several places — Rust can aggregate in one query); connection pooling
  (sqlx) vs PDO-per-process.
- **Don't over-tune early**: verify parity first (M2–M6), then optimize hot
  paths with EXPLAIN + benchmarks; add a CI perf gate in M9 to catch regressions.

### 2.7 Process rules (from the research, adopted)

1. Every milestone: **demo + diff-harness green** before starting the next.
2. Oracle stays live and pinned for the whole project; fixtures regenerable.
3. Port order = **usage census order** (hot ops first), adjusted for the
   read→write→background ladder.
4. Any feature may be marked **port / simplify / drop** with a written note;
   drops are surfaced to the user at each milestone review (feature-parity
   trap discipline).
5. Non-determinism normalizers are reviewed against the *intent* of the output
   (API docs + UI) so the harness can't bless a wrong answer.
6. Security-critical modules (sanitizer, SSRF, auth, crypto) get fuzz +
   interop-vector tests *in the milestone that ships them*, not at the end.
7. Weekly-ish checkpoint summaries: what's demoable, harness status, diff
   backlog, scope decisions needed.

---

## Sources

1. Microsoft Azure Architecture Center — Strangler Fig Pattern: https://learn.microsoft.com/en-us/azure/architecture/patterns/strangler-fig
2. Laminas — The Strangler Fig Pattern (MVC→middleware migration): https://getlaminas.org/blog/2025-08-06-strangler-fig-pattern.html
3. Agile Alliance — "A Tale of Slicing and Imagination" (walking skeleton experience report): https://agilealliance.org/resources/experience-reports/a-tale-of-slicing-and-imagination/
4. Tech Lead Handbook — Feature slicing / steel thread: https://techleadhandbook.org/agile/feature-slicing/
5. DZone — How To Rewrite a Huge Codebase (OpenAPI-stub approach): https://dzone.com/articles/how-to-rewrite-a-huge-code-base
6. Software Engineering Stack Exchange — rewriting a large codebase (1:1 port cost): https://softwareengineering.stackexchange.com/questions/362848
7. Martin Fowler — Feature Parity (Patterns for Displacing Legacy Systems): https://martinfowler.com/articles/patterns-legacy-displacement/feature-parity.html
8. Wikipedia — Characterization test (Feathers / golden master): https://en.wikipedia.org/wiki/Characterization_test
9. Understand Legacy Code — Characterization vs regression vs approval tests: https://understandlegacycode.com/blog/characterization-tests-or-approval-tests/ ; key points of *Working Effectively with Legacy Code*: https://understandlegacycode.com/blog/key-points-of-working-effectively-with-legacy-code/
10. GoReplay — record/replay HTTP traffic: https://goreplay.org/
11. Microsoft Code-with-Engineering Playbook — Shadow Testing (Diffy premise): https://microsoft.github.io/code-with-engineering-playbook/automated-testing/shadow-testing/
12. Twitter (archived) — Diffy: https://github.com/twitter-archive/diffy
13. EasyBI — Parallel-Run Playbook (strangler + parallel run, timeline risk stats): https://www.easy.bi/blog/parallel-run-legacy-migration-playbook/
14. Software Modernization Intelligence — Data Migration Rollback Planning (parallel-run window): https://softwaremodernizationservices.com/insights/database-migration-rollback-planning/
15. AWS Prescriptive Guidance — Database migration cutover: https://docs.aws.amazon.com/prescriptive-guidance/latest/strategy-database-migration/cut-over.html
16. tt-rss Installation Guide (docker topology, env, images): https://tt-rss.org/docs/Installation-Guide.html
17. Docker Hub — supahgreg/tt-rss: https://hub.docker.com/r/supahgreg/tt-rss/ ; supahgreg/tt-rss-web-nginx: https://hub.docker.com/r/supahgreg/tt-rss-web-nginx/
