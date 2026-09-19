# Project Instructions

Standing working agreements for **ttrss** (a Rust re-implementation of the
Tiny Tiny RSS server backend); they apply to every session in this directory.
The frozen PHP reference in `tt-rss/` (commit `2f7d831`) is the spec.

## Load-bearing rules

- **The PHP reference is the spec.** Where the port and `tt-rss/` disagree, the
  PHP wins; surface any deliberate deviation.
- **Verify APIs against the toolchain and vendored sources, not memory.** This
  is a **future-dated Rust** (`rustc 1.98.0`, 2026); `std` and crate APIs may
  differ from training data. Before relying on any `std`/crate API, grep the
  vendored source or sysroot: vendored crates under
  `~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/`, `std` under
  `$(rustc --print sysroot)/lib/rustlib/src/rust/`. Versions come from
  `Cargo.lock` — check the matching copy, not the latest release.
- **sqlx 0.7 decodes strictly — match the column width.** id/uid columns are
  `integer` (INT4) → decode as `i32` (widen at the boundary); `COUNT(*)` is
  `bigint` → `i64`. Never `query_scalar::<_, i64>` an INT4 column (runtime
  `ColumnDecode` mismatch). Keep `statement_cache_capacity(0)` in
  `make_db`/`Db::connect_url` for the same reason — do not re-enable it.
- **Stay on the approved dependency versions** (e.g. sqlx 0.7) unless told
  otherwise. Docker is an acceptable requirement.
- **Keep the repo relocatable: no absolute paths.** Everything under the repo (scripts, build files, docs) references paths **relative to the repo base directory** — resolve them from the script's own location (`$(dirname "$0")/…`) or the current working dir. Never hardcode an absolute path like `/home/<user>/…`; that's what lets the repo be renamed or moved without breakage.

## Working style

- **Surface your own decisions; report them at turn end.** For judgment calls
  the spec doesn't pin down (a behavior detail, a formatting/structural choice,
  a scoping call, an optimization, a "clean up while I'm here" tweak), don't
  silently pick one. Flag it in-line — especially anything that deviates from
  the spec — and end the turn with a short **Decisions to revisit** list (each
  with a one-line rationale and concrete effect).
- **Get to root cause; no whack-a-mole** — when a fix reveals another symptom,
  find the underlying cause rather than patching one at a time.
- **Prefer iterative over recursive.** Refactor recursion into an explicit
  `Vec` work stack (or loop); keep it only where recursion is clearly the most
  readable form (a trivial constant-bounded case). In Rust recursion —
  especially `async fn` (forces `Box::pin`) — hides control flow and risks deep
  stacks.
- **Write in small, verifiable blocks** — one function or test at a time,
  checking in (`cargo check`/`cargo test`) between steps so a failure points at
  a small, isolated change.
- **Run expensive commands once, grep the saved output many times** — capture
  stdout (`tee /tmp/test_out.txt`) and grep it for follow-ups instead of
  re-running `cargo test`.
- **Keep the workspace green**: `cargo test` passes and the tree is clean
  before committing.
- **Address processes by PID, not by name.** Never `pkill`/`killall` unless
  absolutely necessary: names/patterns aren't unique and are susceptible to
  grep misinformation (the shell running the command matches its own pattern).
- **Don't delete outside the working directory without permission.** Only remove files inside the repo (and `/tmp` files you created). Anything outside the repo — other dirs, `~/Downloads`, system paths, etc. — needs explicit permission first.

## Build on existing libraries

- Prefer a maintained crate over hand-rolling; if you're writing boilerplate a
  library already provides, stop and find the crate.
- To learn a library, clone its repo and read in this order: docs → examples →
  tests (they lay out expected behavior) → source. Web-scraping is brittle;
  use it only to locate, then read the cloned source.

## Cloning repos reliably

- Don't guess repo names — read the `repository` field of the vendored crate's
  `Cargo.toml` (exact version in use). E.g. `sqlx 0.7.4` → `launchbadge/sqlx`
  (→ `transact-rs/sqlx`).
- GitHub's git endpoint returns **401 "Repository not found"** for any
  nonexistent/private repo; a **200** means public and cloneable with no auth.
  Verify 200 before cloning.
- Set `GIT_TERMINAL_PROMPT=0` so an auth wall fails fast instead of a headless
  askpass hang. No PAT is needed to read public repos.

## Harness verification: ask before assuming byte-exact

- When a target has more than one defensible bar (**byte-exact** vs
  **functionally equivalent**, e.g. syndication feeds), lay out the options and
  get direction before committing — surface the choice, don't assume it.
- **Frontend HTML forms: match structure + visible text, not whitespace.** The
  read-side dialog HTML is fed to a Dojo `dijit.Dialog` via innerHTML, so the
  browser normalizes whitespace before the JS runs; the JS works on the DOM,
  never the raw string. Match element/attribute structure and visible text
  (option labels, captions, the U+2003 em-space indent and U+2015 separator in
  `select_feeds_cats`); tabs/newlines/indent depth are irrelevant. Prefer clean,
  consistently-indented HTML. (Confirmed from `tt-rss/js/PrefFeedTree.js`
  editMultiple + `SingleUseDialog.js`.)

## Debugging the live UI

For UX-level truth (rendered DOM, XHR bodies, JS errors), drive the app in a
real browser with the MCP `mcp_chrome_*` tools — snapshot for stable node
`uid`s, act by `uid`, read ground truth from the network log and console. The
response **body** (not the HTTP status) is the source of truth. See
`docs/browser-debugging.md`; `browser/diff_rpc_surface.py` diffs the app's RPC
surface against the backend dispatch to spot unhandled (op, method) without
clicking every path.

## Test harness / oracle (retired 2026-09-19)

- The record/replay corpus (`oracle/` + `crates/harness` + the `harness*`
  Makefile targets) is **retired**: it is no longer part of the "green"
  definition for any milestone or increment. The bar is the Rust test suite
  (unit/DB tests, PHP test-parity per the next section) + the demo-able
  server. Do not re-add corpus steps to increment verify bars.
- Why: it covered read-only paths only, and its fixtures decay with
  wall-clock age (the 24 h FEED_FRESH window, relative timestamps, live-feed
  re-fetches) — the maintenance cost outgrew the signal.
- The machinery is kept in-tree, unwired from the gate, in case the
  differential approach is revived; `oracle/README.md` carries the note.

## Test parity (port the tests, not just the code)

When porting a PHP module, **replicate its PHP test cases 1:1** — the PHP suite
is the spec and lands *with* the code, not after. Mirror each matching PHP
`#[test]` into Rust (same inputs, same expected values):

- PHP tests: `tt-rss/{tests,tests_functional,tests_integration,tests/integration,tests/mocked}/`;
  `docs/plan.md` (the test-parity board) tracks the PHP-suite → Rust-module map.
- A PHP case the port omits is kept as `// TODO(port): …` or a named `NOTE`,
  never silently dropped. A broken PHP test is ported with the corrected
  expectation + a `NOTE`.
- DB-backed tests must **not** depend on the mutable golden dump
  (`oracle/data/golden-db.sql`); use the stable schema + fixtures
  (`crates/test-db`). The golden dump is reserved for the end-to-end oracle.

## Context management

If you find a session dump in the local filesystem (probably `~/.pi/`), you can
read the full pre-compaction thread — useful to recover exact steps lost in
summarisation.
