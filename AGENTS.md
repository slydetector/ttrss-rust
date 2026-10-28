# Project Instructions

Working agreements for **ttrss** (a Rust re-implementation of the Tiny Tiny RSS server backend). The PHP reference in `tt-rss/` (git submodule) is the spec. Fresh clone: `make bootstrap`.

## Load-bearing rules

- **The PHP reference is the spec.** Assume the port is wrong — check `tt-rss/` before treating the port as canonical. Deliberate deviations are fine (better fit, more modern, strategic); surface them.
- **Verify APIs against the toolchain, not memory.** Future-dated Rust (`rustc 1.98.0`); `std`/crate APIs may differ from training data. Grep the vendored source: crates under `~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/`, `std` under `$(rustc --print sysroot)/lib/rustlib/src/rust/`. Use the `Cargo.lock` version.
- **sqlx decodes strictly.** INT4 → `i32`, `COUNT(*)` → `i64`. IDs are i32 end to end; i64 only at the JSON/session edge. Keep `statement_cache_capacity(0)` in `make_db`/`Db::connect_url`.
- **Keep dependency versions current.** No pinned majors: bump to the latest stable when doing dep work, and surface major bumps (API churn, re-baselined expectations) as decisions. Docker is acceptable.
- **No absolute paths in the repo.** Resolve from the script's location (`$(dirname "$0")/…`) or the CWD.

## Working style

- **Do not commit (or push) without the user's review.** Green is a precondition, not a substitute: stop with a diff summary + proposed message; commit only on approval.
- **Commit messages: short.** Imperative subject; body = the decision/deviation and its scope, a few lines max. No XML examples, no per-file tours, no test counts or "clippy clean" — the user judges those from the diff summary.
- **Surface your own decisions.** Flag judgment calls inline — especially spec deviations — and end the turn with a short **Decisions to revisit** list (one-line rationale + effect).
- **Root cause, no whack-a-mole.**
- **i18n helpers: import bare, no space before the paren.** `use crate::i18n::__;` then `__("…")`. No qualified `crate::i18n::__ (…)` form, no `f (x)` space. The pipeline mangles `__` in *display* (and str_replace can rewrite it) — build/replace via Python `chr(95)*2`, verify by bytes.
- **Prefer iterative over recursive.** Explicit `Vec` work stack; recursion only for trivial constant-bounded cases.
- **Write in small, verifiable blocks.** One function/test at a time; `cargo check`/`cargo nextest run` between steps.
- **`make fmt` and `make clippy` (zero warnings) before committing; never `cargo fmt` with file args** (loses edition → tree-wide churn).
- **Run expensive commands once.** `tee` the output; grep it for follow-ups.
- **Run tests with `cargo nextest run`** (never bare `cargo test` — compiles ~50 integration binaries, ~5 min). `-p ttrss` for one crate; `-E 'test(...)'` to filter. Fall back to `cargo test` only when nextest can't express the target (e.g. `--doc`).
- **Keep the workspace green:** `cargo nextest run` passes and the tree is clean before committing.
- **Address processes by PID, not by name.** No `pkill`/`killall` — patterns match the invoking shell itself.
- **Don't delete outside the working directory without permission.** The repo and your own `/tmp` files are fine.
- **Mark incomplete work:** `// TODO(scope): …` plus a `tracing::warn!` of the same text. No placeholders that look done.
- **Trust the code, not the ✅.** Verify plan "done"s against the code.

## Build on existing libraries

- Prefer a maintained crate over hand-rolled boilerplate.
- **Best fit, not "already in the workspace."** Compare API shape, faithfulness to the reference, foundations, and new-dependency cost. Adding a new crate is a decision to surface.
- Learn a library by cloning its repo: docs → examples → tests → source.

## Cloning repos

- Don't guess repo names — read the vendored crate's `Cargo.toml` `repository` field (exact version in use).
- GitHub git endpoint: **200** = public, cloneable without auth; **401** = nonexistent/private. Verify 200 first.
- `GIT_TERMINAL_PROMPT=0` so an auth wall fails fast. No PAT needed for public repos.

## Harness verification

- More than one defensible bar (**byte-exact** vs **functionally equivalent**) → lay out the options, get direction.
- **Frontend HTML forms: match structure + visible text, not whitespace.** Dialog HTML is fed to Dojo via innerHTML; the browser normalizes whitespace. Match elements/attributes and visible text (the U+2003 indent and U+2015 separator in `select_feeds_cats`); tabs/newlines/indent depth are irrelevant.

## Debugging the live UI

Drive the app with MCP `mcp_chrome_*` (connects to :9222):

```sh
google-chrome --remote-debugging-port=9222 --user-data-dir=/tmp/chrome-mcp-profile \
  --no-first-run --no-default-browser-check --disable-infobars \
  --disable-session-crashed-bubble --disable-features=Translate <url>
```

One-time per profile: merge `{"credentials_enable_service": false,
"profile": {"password_manager_enabled": false}}` into
`/tmp/chrome-mcp-profile/Default/Preferences` while Chrome is stopped.
Snapshot for stable node `uid`s, act by `uid`; ground truth is the network log and
console — the response **body**, not the status. Dojo: drive widgets via
`dijit.byId(id).set('value', …)`; the codemode sandbox has no `setTimeout` — delay
in-page. See `docs/07-browser-debugging.md`; `browser/diff_rpc_surface.py` diffs
the app's RPC surface against the backend dispatch.

## Test parity

Porting a PHP module means **mirroring its test cases 1:1** — same inputs, same expectations, landing with the code.

- PHP tests: `tt-rss/{tests,tests_functional,tests_integration,tests/integration,tests/mocked}/`; the board is `docs/01-plan.md`.
- A case the port omits → `// TODO(port): …` or a named `NOTE`, never silently dropped. A broken PHP test → corrected expectation + `NOTE`.
- DB tests use the stable schema + fixtures (`crates/test-db`), not the mutable golden dump.

## Context management

Session dumps live under `~/.pi/` — read the full pre-compaction thread to recover steps lost to summarisation.
