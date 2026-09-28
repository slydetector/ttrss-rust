# Update Engine Architecture (M6) — decision & research record

*Status: working decision record. Supersedes the M6 plan's
"golden-file parity of an update run" exit criterion and its implied
separate-daemon framing. The frozen PHP reference (`tt-rss/`, commit
`2f7d831`) remains the behavioral spec; this document records how we
*port* the update engine, not what it must compute.*

Two decisions are settled here:

1. **Verification** — drop the write-heavy "golden update-run" diff; lean on
   the ported PHP unit/functional tests for the deterministic pieces plus a
   lightweight batch smoke test.
2. **Process architecture** — host the web server **and** the update engine in
   **one process / one tokio runtime**, instead of the PHP's separate
   long-running daemon.

---

## 0. Context: what the PHP does, and *why*

The PHP update system is a **multi-process** design, a direct consequence of
PHP having no green threads and no efficient async. It reaches for the only
tools the language offers:

- `update_daemon2.php` — a master process that `pcntl_fork()`s up to
  `DAEMON_MAX_JOBS` (default 2) worker processes; each worker is a one-shot
  batch (`update.php --daemon-loop`), runs, exits; the master re-forks every
  `DAEMON_SLEEP_INTERVAL` (min 60s) and reaps via `SIGCHLD`/`pcntl_waitpid`,
  SIGKILLing any worker past `DAEMON_MAX_CHILD_RUNTIME` (1800s).
- `update_daemon_common()` — even inside a worker, re-invokes
  `passthru(PHP update.php --update-feed:N)` **per feed** (a fresh process
  per feed).
- `flock()` **file lockfiles** coordinate across processes:
  `update_daemon.lock` (one daemon), `update_daemon-PID.lock` (per worker),
  `update.lock` (single-process mode).
- A **DB-level claim** is the real "don't clobber a feed mid-update"
  mechanism: the selection query skips feeds with
  `last_update_started < NOW() - INTERVAL '10 minute'`, and the batch sets
  `last_update_started = NOW()` *before* starting. This survives every process
  boundary.
- A **disk cache** (`DiskCache`, keyed by `sha1(feed_url)`) and the
  **stampfile** (`update_daemon.stamp`) exist largely because the daemon and
  the web server are *separate processes* that must share state via the
  filesystem.

Strip the mechanism and the design encodes a small set of invariants:

| Invariant | PHP mechanism |
| ------------------------ | ------------------------------------------ |
| Don't update a feed in flight | `last_update_started` DB claim (10-min stale) |
| Only one daemon instance | `update_daemon.lock` (`flock`) |
| One bad feed can't kill the updater | per-feed process isolation + error capture |
| Bounded concurrency | `DAEMON_MAX_JOBS` |
| Periodic + batched | `DAEMON_SLEEP_INTERVAL` + `DAEMON_FEED_LIMIT` (500) |
| Least-recently-updated first | `ORDER BY last_updated NULLS FIRST` |
| Housekeeping runs periodically | "task 0" runs `housekeeping_common()` |
| Liveness / observability | stampfile + log level |

Those invariants are what we must preserve; the processes, forks, and
`flock` files are *how* PHP expresses them, and are free to change in Rust.

---

## 1. Verification: drop the write-heavy golden update-run diff

**M6 plan (as written):** *"golden-file parity of an update run (feed XML
fixtures → resulting DB rows) vs PHP."*

**Decision: drop it.** Rationale:

- It is **write-heavy**: an update run produces thousands of rows across
  `ttrss_entries`, `ttrss_user_entries`, the counter-cache tables,
  `ttrss_enclosures`, plus per-feed metadata. A row-by-row DB diff is
  expensive to build and maintain.
- **Low signal-to-noise**: most of that output is "correctly-parsed articles"
  (the boring 95%). The behavior that actually diverges — a dedup decision, a
  filter action, a counter off-by-one, a GUID fallback — is a small fraction.
  A whole-DB diff is dominated by noise.
- **The deterministic pieces are already spec'd by the PHP's own tests**,
  which we port 1:1 per the test-parity rule: `FeedParserTest`,
  `FeedItem{,Atom,Common}Test` (pure), `RSSUtilsTest`, and the counter/filter
  cases. Those are cheap, pure, and are *the* spec for the pieces.

**What replaces it:**

1. **Ported PHP unit/functional tests** for the deterministic pieces
   (parser, article hash, GUID selection/fallback, dedup, filter evaluation,
   counter math). High-signal, already our standard, no DB golden dump needed.
2. **A lightweight batch smoke test**: feed the pipeline a fixture *batch* of
   feeds, then assert a handful of aggregate / cross-feed invariants (e.g.
   expected total new entries, a specific known article present with the
   right tags/score, counter-cache totals, one dedup hit). This catches
   *wiring* and *emergent/cross-feed* bugs — the only thing a whole-DB diff
   uniquely provided — at a fraction of the cost.
3. **The read-side golden harness (record/replay/diff) is unaffected.** It
   compares a *response* (cheap, high-signal) and stays. Only the write-side
   golden *run* is dropped.

> **Nuance / honest caveat:** a full-DB golden run *would* have caught
> cross-iteration / cross-feed emergent behavior for free. We accept that risk
> and cover it with the batch smoke test (item 2) rather than a full diff. If
> the smoke test proves too coarse in practice, the fallback is to assert a
> *normalized subset* of the DB (entries id/title/hash + counter tables) rather
> than every column — still far cheaper than the original plan.

---

## 2. Process architecture: one process hosting web + update engine

**Question.** The web server we've already built (`crates/ttrss`, a
long-running axum app on tokio + sqlx-async) was conceptually kept separate
from the "update daemon" (the M6 plan describes `tt-rss update` /
`tt-rss daemon` subcommands). For simplicity, do we **combine everything into
one process that hosts both**?

**Decision: yes.** The server mode embeds the update engine as background
tokio tasks in the same runtime.

### 2a. Why this is cheap *given where we already are*

The PHP's separation was a **constraint**, not a design choice: PHP-FPM/Apache
serves each web request as (conceptually) its own process, so a long-running
daemon *had* to be separate and share state via DB + filesystem. Our Rust
design does **not** have that constraint:

- `crates/ttrss` is already a **long-running** axum process (not per-request
  like PHP-FPM) — it *can* host background work natively.
- We already use **`tokio`** (rt-multi-thread, time, signal), **`sqlx` async**
  (one pool), and **`reqwest`** (the fetch layer). A combined design needs
  exactly this substrate, which we already have.

So combining is **adding background tasks to the existing runtime**, not a
re-architecture. Nothing we've built is thrown away.

### 2b. Research-verified pros

| Pro | Notes / source |
| --- | --- |
| **Simplicity** | One binary, one lifecycle, one log stream, **no IPC**, no inter-process `flock` coordination, no "is the daemon running?" dance. |
| **Direct in-process triggering** | Web-triggered updates (on-demand refresh, `Feeds::add`/subscribe's immediate first update, `updateFeed`/`subscribeToFeed`) call the same pipeline in-process. In the PHP these had to coordinate with the daemon or run inline in the request. This is the big win for M6. |
| **Shared resources** | One `sqlx` pool, one `reqwest` client, one in-memory feed/icon/DiskCache. Removes the PHP's need for a **disk cache as inter-process IPC** (we can keep the disk cache for durability but not for sharing). |
| **It's the idiomatic tokio pattern** | A single runtime hosts request handlers *and* background/scheduled tasks. Reference: "an hourly job next to an Axum server, on one runtime" via `tokio_cron_scheduler` ([Cronuru](https://cronuru.com/guides/axum-actix-scheduled-tasks)); axum runtime discussion — "tokio will schedule things across threads" in one runtime ([axum #2501](https://github.com/tokio-rs/axum/discussions/2501)). |
| **Precedent for one process doing heavy background work** | `rust-analyzer` is a single-process LSP daemon that does heavy background work (indexing, analysis) alongside serving, kept responsive by offloading CPU work to a thread pool; it isolates only the genuinely subprocess-bound pieces (external `rustc` flycheck, proc-macro servers). Many self-hosted Rust/Go services run serve+background in one process. |
| **Deployment** | Single container / single unit. |

### 2c. Research-verified cons / risks (and mitigations)

| Risk | Why it's real | Mitigation |
| --- | --- | --- |
| **Resource contention** (updates degrade web latency) | The update engine is I/O + CPU heavy (fetches, XML parse, html5ever sanitize, large DB writes). "Scaling vertically (more threads per process) improves CPU utilization but **risks contention**" ([Layrs](https://layrs.me/course/hld/05-application-architecture/background-jobs-overview)); tokio's own guidance is *don't block the runtime* (hence `spawn_blocking`). | Bounded concurrency (`Semaphore`); `sqlx` pool sized to serve both; CPU work (parse/sanitize) on a **bounded** `spawn_blocking` pool; per-fetch timeout. This is the main real risk and it's well-understood. |
| **Scaling asymmetry** (horizontal web tier → N daemons) | "If you have multiple instances of a background job, they might compete for access to resources" ([Azure Well-Architected](https://learn.microsoft.com/en-us/azure/well-architected/design-guides/background-jobs) → leader election / competing consumers). If we ever run N server processes for load, each would run the updater. | Keep the `last_update_started` DB claim (already planned) so N instances naturally dedupe, or elect one "updater" instance. **Non-applicable at tt-rss's single-instance scale**, but the design must not forbid it. |
| **Failure coupling** (updater bug/hang affects web server) | One process, one fate. | Per-update is an independent async task with a `timeout`; a bad feed fails *that task* (a `Result`/panic contained to it), the runtime keeps serving. Rust's memory safety means no segfault to take down the process — far lower risk than the PHP's need for per-feed process isolation. |
| **Restart coupling** | Restarting takes down both. | Trivially acceptable for a self-hosted app; the updater auto-resumes on restart. |
| **Memory / blast radius** | One process holds the peak of both. | Bounded concurrency; stream feed bodies rather than buffering whole feeds in memory beyond need. |

### 2d. Verdict

For a **single-instance, self-hosted** app like tt-rss, **combine**. The
one structural risk that argues for separation (independent horizontal
scaling of the worker tier) simply doesn't apply at tt-rss's scale, and the
`last_update_started` claim keeps that door open. Every cost of the PHP's
separation (IPC, `flock`, disk-cache-as-IPC, daemon-existence checks) is
**eliminated** by combining, and we gain direct in-process update triggering.

**Keep the option open** (low cost): the binary still exposes the CLI
subcommands (`tt-rss update --feeds` one-shot, `tt-rss daemon` standalone) for
M8 CLI parity and for operators who want a separate updater; the DB claim
makes multiple instances safe.

---

## 3. Resulting concurrency design

One `tt-rss` process, one `#[tokio::main]` runtime:

```text
tt-rss  (server mode embeds the updater)
│
├─ axum router  →  frontend API / public / static   (request handlers)
│
└─ updater  (background tokio task, tokio::time::interval every SLEEP_INTERVAL)
     ├─ SELECT due feeds        (same query as RSSUtils::update_daemon_common)
     ├─ claim: UPDATE ttrss_feeds SET last_update_started = NOW() WHERE feed_url IN (…)
     ├─ dispatch per-feed async tasks, bounded by a Semaphore (concurrency N)
     │     ├─ reqwest fetch                              (async I/O)
     │     ├─ spawn_blocking { parse + sanitize }        (CPU → blocking pool)
     │     ├─ dedup / hash / upsert / filter / counters  (sqlx async)
     │     └─ tokio::time::timeout  (per-feed bound)
     └─ periodically: housekeeping_common (the "task 0" work)
```

Idioms (all present in our dependency set):

- **`JoinSet`** — structured, bounded set of in-flight feed-update tasks
  (drain/await on shutdown).
- **`Semaphore`** — cap concurrent feed updates at `N` (the
  `DAEMON_MAX_JOBS` equivalent; can default *higher* than 2 since tasks are
  cheap and I/O-bound).
- **`spawn_blocking`** — XML parse + html5ever sanitize are CPU-bound; run
  them off the async runtime ("occupies a thread from the runtime's blocking
  thread pool").
- **`tokio::time::interval`** — the scheduler loop.
- **`tokio::time::timeout`** — per-feed bound (fetch already has
  `FEED_FETCH_TIMEOUT` = 45s).
- **`tokio::signal`** — graceful shutdown (replaces SIGINT/SIGTERM/SIGCHLD; no
  SIGCHLD since there are no children).
- **One optional cross-process daemon lock** (operational safety / multi-instance).

### PHP mechanism → Rust equivalent

| PHP                                     | Rust                               | Keep / Replace            |
| --------------------------------------- | ---------------------------------- | ------------------------- |
| `pcntl_fork` master/worker              | `JoinSet` of async tasks           | **Replace**               |
| `passthru` re-spawn per feed            | async task per feed                | **Replace**               |
| `flock` worker lockfiles                | in-process channels / `Arc<Mutex>` | **Replace**               |
| `flock` `update_daemon.lock`            | one optional cross-process lock    | **Keep** (1)              |
| `last_update_started` claim             | same DB column + 10-min stale rule | **Keep**                  |
| `check_ctimes` stuck-kill               | `tokio::time::timeout` per task    | **Replace**               |
| `SIGINT/TERM/CHLD` handlers             | `tokio::signal` graceful drain     | **Replace**               |
| `update_daemon.stamp` watchdog          | optional activity file             | **Optional**              |
| `DAEMON_MAX_JOBS`                       | semaphore permits (`N`)            | **Keep** (default higher) |
| `DAEMON_SLEEP_INTERVAL` / `_FEED_LIMIT` | interval + batch limit             | **Keep**                  |
| `update_daemon_common` selection SQL    | same query                         | **Keep**                  |
| "task 0" housekeeping                   | periodic housekeeping task         | **Keep**                  |
| `DiskCache` (disk, shared)              | in-memory cache (disk optional)    | **Simplify**              |

**Dedup in the single process:** the periodic loop selects distinct
`feed_url`s, so it won't double-update within a cycle. The web-triggered
(on-demand) path dedups against the loop via the same in-flight marker
(`last_update_started`, or an in-process `Arc<Mutex<HashMap<feed_url, _>>>`).
The DB column is retained for faithfulness, multi-instance safety, and
visibility.

### Current runtime baseline (as-is today)

Concrete numbers the §5 sizing decisions are anchored to (the web-only server,
before M6):

- **Runtime:** `#[tokio::main]` (bare, `crates/ttrss/src/main.rs`) →
  **multi-thread**; worker threads = `available_parallelism()` (all cores / the
  cgroup limit). Not tuned.
- **Request concurrency:** **unbounded** — no `Semaphore`, no router-level cap,
  no explicit hyper connection limit. Only limited by memory + OS file
  descriptors.
- **CPU-bound work:** none offloaded — no `spawn_blocking` anywhere yet; all
  handler work (md5, small HTML) runs on the async worker threads.
- **DB pool:** `max_connections(10)` (`crates/ttrss/src/core/db.rs`), default 30s
  `acquire_timeout`, no `min_connections`. **This is the effective throughput
  ceiling today** (every request does DB work) — not the CPU or the request count.
- **Outbound HTTP:** reqwest pool — not yet in the request path (M6 feed fetch).

So M6 *adds* the bounding that doesn't exist today: a per-feed `Semaphore`,
`spawn_blocking` for parse/sanitize, and a **larger / re-partitioned sqlx pool**
(§5, item 4) — the update engine's heavy batched writes would otherwise share
the 10-connection pool with web requests.

---

## 4. How this reshapes the M6 plan

- **Drop** the "golden-file parity of an update run" exit criterion → piece
  tests + batch smoke test (Section 1).
- **"tokio workers"**: confirmed correct at the top level.
- **Reconsider "a thin custom parser over a pull parser"** — the PHP
  `FeedParser` is actually `DOMDocument` + `DOMXPath` with registered
  namespaces; a namespace-aware **DOM** crate (e.g. `roxmltree`) fits, with
  the bounded set of queries ported as explicit tree walks. (See
  `docs/01-plan.md`, M6; and the separate parser decision.)
- **Decouple three layers** so the architecture choice doesn't block progress:
  - **M6a — parser** (`FeedParser` + `FeedItem_*`): pure, testable,
    architecture-independent.
  - **M6b — deterministic pipeline** (GUID/hash → dedup → upsert → filter →
    counters): the behavior we verify via piece tests + batch smoke;
    architecture-independent, runnable against a feed-XML fixture → DB rows.
  - **M6c — daemon/scheduler shell** (tokio worker loop, fetch, housekeeping,
    graceful shutdown, the optional daemon lock): the concurrency shell.
- **Fetch layer is the only non-deterministic piece.** For the batch smoke
  test, **replay recorded feed XML** (bypass the network) so results are
  deterministic.
- **Server mode embeds the updater** (Section 2); CLI subcommands (M8) keep
  one-shot / standalone options; the DB claim keeps multi-instance safe.

---

## 5. Decisions to revisit

1. **Concurrency default `N`.** PHP is ~2-way parallel. In async Rust we can
   default higher (e.g. 8–16) since tasks are cheap and I/O-bound. Affects
   throughput/latency, not correctness.
2. **Keep the multi-process option?** Recommend: keep the one daemon lock +
   CLI subcommands (near-zero cost, preserves the scaling door). Or drop for a
   strictly single-process design.
3. **Stampfile.** Recommend: **drop** (a Rust daemon + systemd/watchdog is
   usually enough) unless we want watchdog parity.
4. **`sqlx` pool sizing for the combined process.** Decide the pool size /
   max-overflow so the updater and web requests share it without starving each
   other.
5. **On-demand update path.** Decide whether a web-triggered "update now"
   blocks the request until done, or spawns a background task and returns
   "started" (the PHP tends toward the latter for the daemon, inline for some
   ops).
6. **Disk cache: keep vs. in-memory.** We no longer need the disk cache as
   inter-process IPC. Decide whether to keep a disk cache at all (durability /
   `MAX_CACHE_FILE_SIZE`) or go in-memory per process.

---

## Sources

- tt-rss (frozen reference, commit `2f7d831`): `update_daemon2.php`,
  `update.php`, `classes/RSSUtils.php` (`update_daemon_common`,
  `update_rss_feed`, `housekeeping_common`), `classes/Config.php`,
  `classes/UrlHelper.php`, `classes/FeedParser.php`.
- DeepWiki orientation (read, then superseded by primary sources above):
  <https://deepwiki.com/tt-rss/tt-rss/3-feed-processing-system> (+
  3.1–3.4 subsections).
- Tokio background-task / axum pattern:
  <https://cronuru.com/guides/axum-actix-scheduled-tasks>;
  <https://github.com/tokio-rs/axum/discussions/2501>.
- `tokio::spawn_blocking` semantics:
  <https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html>.
- Worker scaling trade-off (process vs. thread, contention):
  <https://layrs.me/course/hld/05-application-architecture/background-jobs-overview>.
- Multiple background-job instances competing for resources (leader election /
  competing consumers):
  <https://learn.microsoft.com/en-us/azure/well-architected/design-guides/background-jobs>.
- `rust-analyzer` parallelism as a separate concern on a single-process core:
  <https://github.com/rust-lang/rust-analyzer/issues/17491>.
