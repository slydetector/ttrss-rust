# Project Instructions

These are standing working agreements for the **tt-rust** project (a Rust
re-implementation of the Tiny Tiny RSS server backend). They apply to every
session in this directory. The frozen PHP reference lives in `tt-rss/`
(commit `2f7d831`) and is the spec.

## Verify against the installed toolchain and vendored sources — not memory

- The installed Rust is a **future-dated build** (`rustc 1.98.0`, 2026). Its
  standard library and dependency APIs may differ from real-world timelines.
  **Never assert an API exists from training data.** Before relying on any
  `std` or crate API, grep the vendored source or the toolchain sysroot:
  - Vendored crate sources: `~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/`
  - Std library: `rustup doc --` or the sysroot under `$(rustc --print sysroot)/lib/rustlib/src/rust/`
- The exact versions in use are declared in `Cargo.lock`; check the matching
  vendored copy, not the latest release.

## Build on existing libraries; do not cleanroom the wheel

- Prefer a maintained crate over hand-rolling equivalent logic. If you find
  yourself writing lifecycle/boilerplate that a library already provides, stop
  and look for the crate.
- To learn how to use a library properly, **clone its git repo** and go in
  this order: (1) its documentation, (2) its examples, (3) its tests (they lay
  out the expected behavior), (4) the actual library source. Web-scrape tools
  (DeepWiki, search snippets) are brittle and lower-fidelity; use them only to
  locate things, then read the cloned source in that order.

## Cloning repos reliably

- Do **not** guess repo names from memory. Read the `repository` field from the
  vendored crate's `Cargo.toml` (the exact version in use) — that is the
  authoritative source. Example: `testcontainers 0.27.3` →
  `testcontainers/testcontainers-rs`; `sqlx 0.7.4` → `launchbadge/sqlx`
  (redirects to `transact-rs/sqlx`).
- GitHub's git endpoint (`...git/info/refs?service=git-upload-pack`) returns
  **HTTP 401 + "Repository not found"** for any nonexistent or private repo
  (by design, to prevent name enumeration). A **200** means a public repo that
  clones with no authentication. Verify the URL returns 200 before cloning.
- Always set `GIT_TERMINAL_PROMPT=0` so an auth wall fails fast and clean
  instead of hanging on the headless `SSH_ASKPASS`/`gnome-ssh-askpass` helper
  (the confusing "unable to read askpass response" error).
- **No PAT is needed** to read public repos. Only add a PAT (or SSH key) if a
  private repo, push access, or higher API rate limits are required.

## Working style

- **Get to root cause; no whack-a-mole.** When a fix reveals another symptom,
  stop and find the underlying cause rather than patching symptoms one by one.
- Keep the architecture clean, isolated, repeatable, and fast.
- **Write code in small, verifiable blocks.** Don't emit hundreds of lines in a
  single tool call — large single-shot writes/edits are brittle (exact-match
  edits fail on any whitespace drift, and a big blob is hard to review or fix).
  Build up files iteratively: write/patch a function or a test at a time, and
  check in (`cargo check`/`cargo test`) between steps so a failure points at a
  small, isolated change.
- Keep the workspace green: `cargo test` must pass and the working tree must be
  clean before committing.
- Docker is an acceptable requirement.
- Stay on the approved dependency versions (e.g. sqlx 0.7) unless told to
  change them.
- **sqlx 0.7 decodes strictly — match the column width:** tt-rss id/uid columns
  are `integer` (INT4) → decode as `i32` (widen at the boundary); `COUNT(*)` is
  `bigint` → `i64`. Never `query_scalar::<_, i64>` an INT4 column (runtime
  `ColumnDecode` mismatch). See the test-infra findings in `docs/plan.md`.
  Also: `make_db`/`Db::connect_url` keep `statement_cache_capacity(0)` for the
  same width-mixing reason — do not re-enable it.

## Test-parity rule

- Port each PHP test case **1:1** (same inputs, same expected values). If a PHP
  test is broken, port it with the corrected expectation and add a `NOTE`
  comment explaining the deviation.
- DB-backed tests must **not** depend on the mutable golden dump
  (`oracle/data/golden-db.sql`); use the stable version-controlled schema +
  fixtures. The golden dump is reserved for the end-to-end oracle harness.
