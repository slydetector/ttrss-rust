# Daemon backlog drain (large-import catch-up) — deferred design record

*Status: deferred. Parked after investigating "OPML import of N feeds takes
N/50 × 2 min to drain". The immediate fix is the zero-code lever in §3; the
"drain mode" (option B) is spec'd in §5 to pick up later if the backlog is
still painful. Nothing here has been implemented.*

---

## 1. Problem

Importing a large OPML file leaves the feeds never-updated (`last_updated
IS NULL`). The update daemon processes **50 due feeds per 120 s tick**, so a
500-feed import takes ≈20 min to fully drain (50 per tick, 120 s apart).

## 2. Findings: where the 50 lives (it is the faithful PHP behavior)

**PHP** (frozen reference `tt-rss/`, commit `2f7d831`):

- `Config::DAEMON_FEED_LIMIT = 50` (default, `classes/Config.php`).
  `RSSUtils::update_daemon_common($limit)` builds the due-feed SELECT ending
  in `LIMIT 50`. **Every** entry point (`--daemon-loop`, `--force-update`,
  `--feeds`, `--daemon`) passes that same constant — no CLI knob.
- The daemon (`update_daemon2.php`) is a **forking master**: every
  `DAEMON_SLEEP_INTERVAL` (120 s, min 60) it spawns up to `DAEMON_MAX_JOBS`
  (2) children; each child runs **one** 50-feed pass and exits.
- PHP **pre-claims the whole batch before fetching**
  (`UPDATE ttrss_feeds SET last_update_started = NOW() WHERE feed_url IN (…)`,
  comment: *"minimize collision risk for parallel daemon tasks"*), so the two
  children per cycle claim **disjoint** batches → PHP drains a backfill at
  **≤100 feeds / 2-min cycle**.
- Newly imported feeds sort first: `ORDER BY last_updated NULLS FIRST` + the
  `last_updated IS NULL` due-disjunct → the backfill drains before
  steady-state feeds.

**Port** (`crates/ttrss/src/updates.rs`, `daemon_loop` / `select_due_feeds` /
`claim_feeds`): faithful to **one PHP task** — one pre-claimed 50-feed batch
per tick, `daemon_max_jobs` (2) as **in-batch concurrency**
(`JoinSet` + `Semaphore`), 120 s tick (min 60 enforced). Env knobs mirror PHP
defaults (`crates/ttrss/src/core/config.rs`): `TTRSS_DAEMON_FEED_LIMIT` (50),
`TTRSS_DAEMON_MAX_JOBS` (2), `TTRSS_DAEMON_SLEEP_INTERVAL` (120).

**Consequence**: the port drains **50 / cycle** vs PHP's **100 / cycle** —
`daemon_max_jobs` is modeled as in-batch workers, not as parallel passes.
This is a pre-existing throughput-modeling gap, independent of the options
below.

## 3. Options (decided-for-now: A)

| Option | Change | Notes |
|---|---|---|
| **A. Raise `TTRSS_DAEMON_FEED_LIMIT`** | **zero code** — one env var (e.g. `=1000`) | Same lever PHP admins use (the constant); spec-faithful; reversible by deletion. Steady state untouched (the limit only caps a batch). Cost: a tick blocks until its batch finishes; `demo-stop` unresponsive mid-batch (true of any batch, incl. PHP). If a future import exceeds the value, raise it again. |
| **B. Drain-until-empty mode** | small change to `daemon_loop` | A tick that finds a never-updated backlog runs back-to-back batches (no sleep) until it's empty, capped + cancel-aware. Spec in §5. |
| **C. Post-import catch-up job** | new machinery | After `opml::import`, fire a one-shot "update these feed IDs now" batch. Most targeted, biggest deviation, most code. Parked. |

**Decided for now: A.** If a 500-feed import still feels slow, A alone gets
it to one ~30–60 min tick; B would remove the *gap* between batches (A's 1000
limit already does that for ≤1000 feeds — so B's remaining value is for
>limit-sized backfills and is the real reason to pick it up).

## 4. Non-goals / constraints

- Default behavior must stay PHP-faithful → any B variant goes behind a gate
  (default off) and gets a `01-plan.md` NOTE.
- Selection, claim (10-min `last_update_started`), ordering, per-feed fetch
  semantics are unchanged by every option — only passes-per-tick differ.
- The claim mechanism gives drains natural progress + backpressure: a batch's
  feeds are invisible to the next select, and a fully-failed batch self-excludes
  for 10 min (same retry cadence as PHP).

## 5. Option B — "drain mode" spec (to pick up later)

**Gate**: `TTRSS_DAEMON_DRAIN=0|1` (default **0**); caps
`TTRSS_DAEMON_DRAIN_MAX_BATCHES=20`, `TTRSS_DAEMON_DRAIN_MAX_SECONDS=1800`.

When enabled, a tick that finds due feeds with `last_updated IS NULL`
(never updated) enters drain mode:

- Back-to-back batches, **no sleep between**: each pass re-runs the unchanged
  `select_due_feeds` (same 50-limit/claim) → claim → process (2 in flight) →
  immediately re-select. Steady-state feeds are never touched by the drain.
- **Stops when** the never-updated set is empty **or** 20 batches **or**
  30 min — whichever first; the rest drains next tick at normal cadence.
- **Housekeeping** (`run_due_tasks`) moves inside the drain loop — runs after
  every batch (PHP-faithful placement: `housekeeping_common` after each
  `update_daemon_common` pass).
- **Shutdown**: `select!` before each new batch **and** `JoinSet::abort_all()`
  on cancel — stops in seconds, not hours.
- **Post-drain**: keep the current ticker (`interval` + `MissedTickBehavior::Delay`)
  → the next tick fires immediately after the drain (a cheap no-op select;
  failed feeds are claim-blocked 10 min, so no tight retry), then 120 s cadence.
- **Logging**: `drain started` (debug) → per-batch debug lines → one **info**
  `drain complete batches=k feeds=N secs=S` (or `drain capped (…)` /
  `drain aborted (shutdown)`).

**Implementation footprint**: entirely inside `daemon_loop`
(`updates.rs:1082`) — a `while` around the existing batch block; plus one new
env field in `core/config.rs`. `select_due_feeds` / `claim_feeds` /
`fetch_one_feed` reused verbatim.

**Tests** (`tests/daemon_loop.rs`): 120 due → 3 batches in one tick (drain on)
vs 1 batch (drain off — existing tests prove the default stays faithful);
batch cap and budget each stop the drain; cancel mid-drain aborts promptly;
steady-state regression (≤50 due → identical to today).

**Behavior math** (limit 50, 2 in-flight, ~3–10 s/feed):

| | today (A off) | A (`=1000`) | B-standard |
|---|---|---|---|
| 500-feed import | 10 ticks ≈ 20 min | 1 tick ≈ 30–60 min | 1 tick ≈ 30–60 min |
| 5000-feed import | 20 min × 100 ticks ≈ 3.3 h | 10 ticks ≈ 20 min | 3 ticks (cap-bounded) |
| steady state | PHP-identical | PHP-identical | PHP-identical |
| shutdown latency worst case | ≤ 1 batch | ≤ 1 batch | ≤ seconds (G2) |

## 6. Open questions (when picking this up)

1. Is A's throughput acceptable for the expected import sizes? (A handles
   ≤1000 feeds in one tick with no code at all.)
2. If B: confirm the trigger scope — "never-updated only" was the recommendation
   (targets exactly the import case); "always" risks a never-sleeping daemon
   whenever due > limit.
3. Independent small fix, worth considering regardless: model
   `daemon_max_jobs` as **parallel passes** (what PHP actually does) instead of
   in-batch concurrency — restores PHP-parity throughput (100/cycle) with zero
   config changes.
