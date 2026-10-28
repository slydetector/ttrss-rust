# Rust Quality Review — `ttrss` server crate

> **Status:** independent code-quality review (read-only, no code changes)
> **Date:** 2026-09-24
> **Toolchain under review:** `rustc 1.98.1` (2026-09-01), workspace declares `edition = "2021"`
> **Scope:** full workspace, weighted to the core crate `crates/ttrss`
> **Lens:** judged *as production Rust* — idiomaticity, architecture/design, and
> sustainability — on its own merits. This review reads the code the way an
> expert Rust reviewer would, without assuming the historical origin. Where a
> smell is evidently a **deliberate, documented tradeoff**, it is labeled as such
> (with its cost) rather than as an oversight.

---

## 1. Executive summary

`ttrss` is a large, working, heavily-tested Rust backend. It is **unambiguously
not a toy**: ~74K LOC of source + tests, an axum HTTP/RPC server, a Postgres
(data) layer, a scheduler/daemon, a dynamic plugin host, i18n, and an
**exceptional verification suite** (~1,400 test functions, 72 integration files,
with differential golden fixtures recorded from a running PHP 8.5 reference).
The documentation is pervasive and thoughtful, and the code is `unsafe`-free.

Its central characteristic is that **the architecture is fidelity-first, not
idiomaticity-first.** It is, structurally, *a Rust re-expression of a legacy
codebase's shape*: a flat, fully-`pub` namespace of ~40 sibling feature modules;
`serde_json::Value` (plus a second hand-rolled `phpser::Value`) as the cross-
layer data currency; 3,000-line "feature-as-single-file" modules; a 41-field
config god-object; no data-access layer (all SQL inline); and a body of
hand-rolled re-implementations of language built-ins to match the reference
byte-for-byte. Each of these is a defensible *strategy for a high-fidelity port*,
but taken together they mean the code **deliberately trades away Rust's two
biggest structural strengths — type-driven design and encapsulation — for
structural parity.** That is the single most important thing a Rust reviewer
would say about this codebase.

So the honest read: **you would trust it in production, and you would learn from
its testing and documentation discipline — but you would not point at it as a
model of idiomatic Rust architecture.**

### Grade (on the requested spectrum)

> **7 / 10 — upper-middle.**
> Bottom of the scale ("a toy / a disgrace") and top of the scale ("expertly
> crafted software I'd use to teach") bracket the range. This code sits firmly in
> the upper-middle: **strong, trustworthy production software with standout
> verification, held back from the top by a fidelity-first architecture that
> departs from idiomatic Rust in ways that will cost maintainability as it grows.**
>
> - Verification & documentation: **9/10** (would be the teaching example).
> - Correctness infrastructure (async, DB session setup, shutdown, crypto): **8/10**.
> - Idiomaticity & architecture: **5.5/10** (deliberate, but a real cost).

---

## 2. Methodology & calibration

**How expectations were set.** Rather than relying on a static mental model of
"expert Rust," expectations were calibrated against current, citable standards:

- **Rust API Guidelines** (rust-lang) — the normative reference for public-API
  hygiene, naming, and conversions.
- **Edition & toolchain reality** — confirmed that current stable (1.98.1,
  Sept 2026) defaults to the **2024 edition** (since 1.85); this project now
  declares **2024** (bumped from 2021; see §4.10). No "2026 edition" exists yet.
- **Async-trait object safety** — confirmed (multiple sources incl. the async-
  fundamentals initiative) that `async fn in traits` desugars to non-object-safe
  opaque futures, so `dyn Trait` + async *requires* a boxed-future return. This
  matters for judging the plugin host (see §4.6).
- **Curated "what good Rust looks like"** — the `hexuria/impeccable-rust` rules
  collection, used as a cross-check for idiomaticity.
- **General software design** (language-agnostic): module boundaries, dependency
  direction, single-responsibility, error-handling philosophy, and the cost of
  dynamic typing in a statically typed language.

**What was sampled.** Representative cross-section across all layers: the crate
root (`lib.rs`), entrypoint (`main.rs`), the data layer (`core/db.rs`,
`core/config.rs`), the RPC surface (`rpc.rs`), a large feature module
(`feeds.rs`, 3,186 LOC), the plugin contract (`pluginhost/plugin.rs`), the i18n
substrate (`i18n.rs`), the PHP built-in port (`util.rs`), the PHP-serializer
(`core/phpser.rs`), plus a broad *pattern census* (regex counts over all of
`src/`) and a sample of the test suite. Quantities below are from that census.

**The lens.** The code was evaluated as Rust. Many of the "smells" below are in
fact *the intended shape of the codebase* — it is a port, and the team has
explicitly, repeatedly chosen structural fidelity to the reference. I have
therefore treated such items as **documented tradeoffs** (good engineering: they
wrote down the rationale and the cost) and graded them on whether the tradeoff
is *wise*, not on whether the author "didn't know better."

---

## 3. The codebase at a glance

| Dimension | Measurement |
|---|---|
| Workspace crates | `ttrss` (server), `test-db` (Postgres test harness) |
| Core crate size | **74.3K LOC** total (47.4K in `src/`, 84 files; ~26.9K in `tests/`) |
| Functions | 1,959 total, **421 async** (~22%) |
| Data layer | thin `Db` wrapper over `PgPool`; **349 inline SQL strings**, no query/repository layer |
| Cross-layer data type | **`serde_json::Value`** (1,067 refs) + a second `phpser::Value` |
| Typed types | 77 `pub struct`, 11 `pub enum`, **2 `pub trait`** |
| Visibility | **1,161 `pub`** vs 71 `pub(crate)` (~94% public) |
| Error surface | ~103 functions returning `Result`; **299 `.unwrap()`/`.expect()`** |
| `unsafe` | **0** |
| Numeric casts (`as i32/i64/…`) | **294** |
| Async boxing (`Box::pin`) | **134** (concentrated in the plugin host) |
| Tests | **~1,425** test fns, 72 integration files, 50 in-crate `#[cfg(test)]` modules |
| Largest files | `feeds.rs` 3,186 · `sanitizer.rs` 2,130 · `headlines.rs` 2,113 · `updates.rs` 1,864 · `rpc.rs` 1,379 |
| Dependencies | notable redundancy in HTML/XML libs (see §4.7) |

The shape, in one line: **a monolith crate of flat `pub` feature modules, each
talking Postgres inline and speaking JSON-`Value` to the RPC layer, glued by a
dynamic plugin host, and held together by an outsized test suite.**

---

## 4. Findings — consistent patterns

Findings are grouped as **recurring patterns** (the ask), not line-by-line nits.
Severity: 🔴 high (architectural, long-horizon cost) · 🟠 medium · 🟡 low.
"Deliberate?" marks items that are the intended shape of the codebase.

### 4.1 🔴 Fidelity-first architecture — "PHP in Rust syntax"
**Pattern.** The whole design mirrors a legacy codebase's *structure* rather than
exploiting Rust's type system. Evidence is consistent across layers:

- **`serde_json::Value` is the data currency** at every external boundary. RPC
  handlers take `&Map<String, Value>` and return `Value`; plugin hooks take/
  return `Value`; 1,067 `Value` references. Typed row structs exist only at the
  SQL boundary (`CatRow`, `FeedRow`), then the data is immediately re-expressed
  as `Value` for the RPC envelope. This is **dynamic typing re-imported into a
  statically typed language** — it discards compile-time guarantees across most
  of the data flow.
- **Two parallel dynamic value systems** coexist: `serde_json::Value` (RPC/plugin)
  and `phpser::Value` (session storage), the latter with a **hand-rolled PHP
  `serialize()`/`unserialize()`** (byte-level `i:…;`, `b:1;`, `N;` encoders).
- **Hand-rolled re-implementations of language built-ins** to match reference
  semantics exactly: `strip_tags` (a character-scanner, not the HTML parser the
  project already depends on), `vsprintf` (a printf interpreter), `php_int_cast`
  (PHP's `(int)` cast), a **custom `application/x-www-form-urlencoded` parser**
  (because stock `serde_urlencoded` collapses repeated `ids[]` keys), and
  `htmlspecialchars`.

**Why it matters.** This is the root of most other findings. The cost is paid in
maintenance: a change to a data shape propagates through `Value` juggling rather
than being caught by the type system; the hand-rolled parsers/serializers are
fragile, hard-to-test surfaces that re-implement what libraries already do.
**Mitigating context (deliberate?): yes** — each re-implementation is documented
with the specific reference behavior it must match and (for the param helpers)
verified by **differential golden fixtures recorded from real PHP 8.5** (§5.1).
That is a legitimate *parity* rationale. But it is a *fidelity* rationale, not an
*idiomatic-Rust* one, and a reviewer should name it as such: **the code is
optimized to behave like the reference, not to be idiomatic Rust.**

### 4.2 🔴 No encapsulation — the crate is ~94% `pub`
**Pattern.** `lib.rs` declares ~40 modules as `pub mod`, `AppState` exposes
`pub config / db / plugins / i18n` (only `prefs_cache` is `pub(crate)`), and a
census finds **1,161 `pub` vs 71 `pub(crate)`**. There is effectively no
internal/public boundary.

**Why it matters.** The Rust API Guidelines' first principle is *keep the public
surface small* and *don't expose internals*. With everything reachable from
everything, there is no seam for refactoring: any test or handler can reach into
any module, so "private by construction" is impossible and every identifier is
accidentally part of the crate's contract. It only "works" because the crate is
consumed by its own binary and tests; but that is precisely where encapsulation
matters most (it is the discipline that keeps a 74K-LOC crate navigable and
safe to change). This is a **general maintainability** concern, not merely a Rust
idiom one.

### 4.3 🟠 God-modules — "feature as one file"
**Pattern.** Features are single flat modules of free functions, several of them
very large: `feeds.rs` **3,186 LOC / ~60 functions**, `sanitizer.rs` 2,130,
`headlines.rs` 2,113, `updates.rs` 1,864, `rpc.rs` 1,379. `feeds.rs` alone
covers feed tree, index, subscribe, batch edits, purge, counters, access keys,
and shared URLs. `rpc.rs` is a **single dispatch module** whose `dispatch()`
funnels every `op`/`method` through one growing `match` (with a 5-variant ad-hoc
`DispatchOutput` enum: `Json | Html | TextHtml | Raw | Download`).

**Why it matters.** Large flat modules hurt navigation, review, and ownership,
and the giant `match` in `rpc.rs` is a classic **open/closed problem**: adding a
handler means editing a central function rather than dropping in a module. The
free-function-in-a-namespace idiom is fine at small scale; at 3,186 lines it
should be decomposed into submodules (`feeds::tree`, `feeds::crud`, `feeds::keys`,
…). **Deliberate?: partly** — it mirrors the reference's file layout, but the
concentrations (3K-line files, one dispatch `match`) are now the port's own
accumulation, not something the reference forces.

### 4.4 🟠 `Config` is a 41-field god-object, hand-built in 70 places
**Pattern.** `Config` is a flat struct of **41 `pub` fields** (no grouping into
`DbConfig`/`DaemonConfig`/`Schedule*`), derives `Debug, Clone` but **not
`Default`**, is built in exactly **one** place (`from_env`), and is then
constructed by **full struct literal in ~70 sites** (almost all in tests).

**Why it matters.** The missing `Default` is the load-bearing problem: every
test that needs a config must spell out ~41 fields (see the 40-line literal in
`core/db.rs`'s test). That makes tests verbose and fragile — adding a field is a
70-site edit. The fix is cheap and high-leverage: `#[derive(Default)]` + a
`Config::for_testing()`/builder, or `..Default::default()` in tests, would
collapse 70 giant literals into small diffs. This is a concrete, high-frequency
**testability** smell.

### 4.5 🟠 No data-access layer — SQL inline, 294 numeric casts
**Pattern.** `Db` is a thin wrapper over `PgPool` (session-setup + pooling); the
actual **349 SQL statements are inline** in feature modules via
`sqlx::query("…")/.bind(…)`/`.fetch_…()`. There is no repository/query layer.
Because the Postgres driver decodes column widths strictly, the code carries
**294 `as i32/i64/usize` casts** at the SQL boundary (INT4→`i32`, `COUNT(*)`→`i64`),
concentrated in `feeds.rs` (52), `api.rs` (24), `settings.rs` (20).

**Why it matters.** Inline SQL is acceptable in Rust, but with no abstraction the
data layer has no home for: width discipline (the casts are a **silent
corruption hazard** — a wrong cast compiles fine and yields wrong values at
runtime), connection/transaction policy, and query testability. The cast density
is a direct symptom of "the type system isn't doing the width work for you."
**Deliberate?: yes** (mirrors the reference's inline-SQL style), and the team
has explicitly documented the width rules — but the *consequence* (294 manual
casts) is a real long-horizon maintenance cost.

### 4.6 🟠 Plugin host: boxed-future `dyn Trait` — correct, but a lifetime/alloc tax
**Pattern.** The plugin contract is a trait whose async hooks return
`BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`, stored as
`Box<dyn Plugin>`, with 134 `Box::pin` sites (56 in `pluginhost/plugin.rs`,
20 in one plugin alone).

**Why it matters — the nuance.** This is **not a bug**: `async fn in traits` is
not object-safe, so a registry of `dyn Plugin` *must* box the futures. The team
documented the rationale (object-safety, `Send` for cross-worker awaits, an
ABI-friendly contract for a future WASM loader). That is the **correct, standard
tool** for the job. The residual cost is real but bounded: every hook signature
carries `<'a, 'h: 'a>` lifetime gymnastics (readability), and each invocation
allocates a boxed future (minor perf). I flag it here only to say: **judged on
idiomaticity it looks dated; judged on correctness it's the right call.** A
future refactor to a static-dispatch (plugin-typed) host would remove the boxing,
but the current choice is defensible.

### 4.7 🟡 Dependency redundancy (HTML/XML)
**Pattern.** `Cargo.toml` pulls in `scraper` + `kuchikiki` + `html5ever` +
`ego-tree` (the first three are the *same* HTML-parsing stack — `scraper`
re-exports `kuchikiki`/`html5ever`), and **two** XML libraries (`quick-xml` and
`roxmltree`).

**Why it matters.** Redundant, overlapping parsers increase build time, surface
area, and the risk of two libraries disagreeing on edge cases. Listing
`scraper` *and* `kuchikiki` *and* `html5ever` is almost certainly at least two
redundant entries. Worth a one-pass dedup.

### 4.8 🟡 Error handling skews to `unwrap`/`expect`
**Pattern.** ~103 functions return `Result`, but there are **299 `.unwrap()` /
`.expect()`** across `src/`. A share of these are legitimate (tests, `Mutex`
locks, validated invariants), but the ratio signals a **panic-leaning** error
culture in the non-test paths rather than error propagation. `anyhow` is the
only error crate (no `thiserror`), so internal errors carry context strings
rather than typed variants.

**Why it matters.** In a long-lived server, an `unwrap()` on a value that can
legitimately be absent is a future crash. The ratio is worth a targeted sweep to
distinguish *provably-invariant* unwraps (fine) from *should-be-`?`* unwraps.
(`anyhow`-only is defensible for a binary crate — there is no public API that
needs typed errors — so the *absence of `thiserror`* is not itself a problem.)

### 4.9 🟡 Documentation hygiene at scale
**Pattern.** Documentation is extensive (a genuine strength — see §5.2), which
is exactly what makes **drift** a risk. Concretely, `util::strip_tags` carries
**two concatenated `///` blocks**: the first describes a character-class
algorithm ("a tag starts with `<` followed by a letter, `/`, `!`, or `?`") that
the code **does not implement** — the actual code is an open/close tag *counter*.
The first block is a leftover from an earlier approach.

**Why it matters.** A doc comment that contradicts the code is worse than no
doc — a future reader will trust the prose and break the algorithm. At this
documentation density, a periodic "do the comments still match?" pass is a
reasonable part of the maintenance model.

### 4.10 ✅ `edition = "2024"` (was `"2021"`) — resolved
**Pattern.** The workspace declared `edition = "2021"`, while the current
default edition (since Rust 1.85, and the baseline for 2026) is **2024**.

**Why it mattered.** Low: being two editions behind the default forgoes the
2024 edition's refinements. The real 2024 changes are (a) `unsafe` fns must
wrap unsafe ops in an explicit `unsafe {}` block (`unsafe_op_in_unsafe_fn`
default-on), (b) `impl Trait` return-position **lifetime capture**, and
(c) `static_mut_refs` warn-by-default. *(The earlier draft of this item loosely
listed "match-ergonomics refinements" — that's 2018 — and "Future/IntoFuture in
the prelude"; neither is a 2024 headline change.)*

**Resolved.** Bumped `[workspace.package].edition` to `"2024"` and adopted the
RPIT idiom: dropped the now-redundant `+ '_` on `HostCtx::active_plugins`'s
`impl Iterator` return (the crate's only RPIT with a lifetime in scope;
`healthz` has none). Verified against the toolchain, not memory: the crate has
no `unsafe fn` / `unsafe {}` / `static mut`, so the two 2024 safety defaults
have nothing to flag, and the bump compiles identically — `cargo check
--workspace` 0 errors/0 warnings; full `cargo test` 78 binaries, 1457 passed,
0 failed.

---

## 5. What's done well

This is where the "PHP-in-Rust" framing has to be corrected, because several
practices are **genuinely expert** regardless of architecture:

### 5.1 🔥 The verification suite is a model (this is the teaching example)
~1,425 test functions, 72 integration files, plus 50 in-crate `#[cfg(test)]`
modules. Two practices stand out:

- **Differential golden fixtures.** `tests/param_differential.rs` pins the Rust
  param-parsing helpers to the reference by **recording expected outputs from a
  running PHP 8.5 CLI** (matching the app's PHP version) via recorder scripts,
  baking them into the test with `include_str!`. The front-end input contract
  and the recorded outputs are both stable, so the fixtures rarely need
  re-recording. This is *differential testing done properly*: it catches the
  exactly-the-quirk behaviors (PHP `(int)` cast, `strip_tags`, sparse-array
  param parsing) that unit intuition would miss.
- **1:1 test-case ports with explicit deviation notes.** Parity tests (e.g.
  `feeds_parity.rs`) port specific named reference test cases and record, in a
  `NOTE`, where the port deliberately diverges (e.g. "the reference accepts a
  raw string id, so the non-numeric case is handled at the handler layer, not
  here"). That discipline — *an omission is a documented `NOTE`, never silent* —
  is exactly the habit that makes a large port trustworthy.

DB-backed tests run on a fresh per-test Postgres (via `test-db` +
testcontainers) rather than a mutable golden dump, so the suite is hermetic.

### 5.2 🔥 Documentation as design communication
Nearly every function documents *what reference behavior it mirrors and why*,
and the plugin contract has an explicit **"Deliberate deviations from PHP"**
section. Infrastructure decisions are documented at the site with their
rationale and cost:
- `core/db.rs`: why the session GUCs are applied in `after_connect` (once per
  new connection) rather than every acquire — with the perf reasoning (the pool
  only `ping`s idle connections; re-running would add four round-trips per query).
- The `statement_cache_capacity(0)` disable is explained as a wire-protocol
  hazard (08P01) from cross-call prepared-statement reuse.
- `main.rs` explains the daemon/scheduler hosting decision and the `TTRSS_DAEMON`
  switch.
This "write down the decision and its cost" habit is the signature of careful
engineering.

### 5.3 Correctness infrastructure
- **Zero `unsafe`.**
- **`tracing` throughout** (only 1 stray `println!`/`eprintln!` in the whole
  crate), structured with context.
- **Graceful shutdown** (SIGTERM/ctrl-C → cancel token → let the update loop
  finish its in-flight batch).
- **Constant-time** string comparison (`ct_eq`) for the places that need it.
- **Task-local i18n** (`tokio::task_local!`) for request-scoped locale instead of
  threading `(locale, cache)` through every function — a clean, idiomatic use of
  the async runtime.
- Known inefficiencies are honestly flagged (e.g. a `TODO(perf)` noting locale
  resolution runs on every RPC op even when nothing renders).

### 5.4 Dependency discipline (mostly)
`reqwest` built with `default-features = false` + `rustls` (no OpenSSL), explicit
feature lists, workspace-level dependency pinning with `resolver = "2"`, and a
`profile.release` with `lto = "thin"`. The main blemish is the HTML/XML
redundancy (§4.7).

---

## 6. Grade — detailed

**Overall: 7 / 10** on the requested spectrum
(*"a toy / a disgrace" → "expertly crafted software I'd use to teach"*).

Breakdown:

| Axis | Score | Rationale |
|---|---|---|
| Verification & test discipline | **9/10** | Differential golden fixtures from a live reference + 1:1 case ports with explicit deviation notes. Above the bar of most production Rust, let alone most code. The thing you'd *teach*. |
| Documentation | **9/10** | Pervasive, decision-carrying, deviation-explicit. Drift risk is the only blemish. |
| Correctness infrastructure | **8/10** | `unsafe`-free, `tracing`, graceful shutdown, task-local i18n, documented infra decisions, constant-time compares. |
| Idiomaticity | **5.5/10** | `Value`-currency I/O, 94% `pub`, god-modules, `Config` god-object, inline SQL + 294 casts, hand-rolled built-ins. Deliberate, but a real cost. |
| Architecture / sustainability | **6/10** | Fidelity-first is a reasonable port strategy; the *consequences* (no seams, no encapsulation, big central `match`, no data layer) are what age it. |

**In plain terms on the spectrum:** this is **not** a toy and **not** a disgrace
— it's a serious, working, well-verified production backend. It falls short of
the top of the scale only because **its architecture is a deliberate mirror of a
legacy codebase rather than an idiomatic Rust design**: it uses the language's
body while behaving like its reference. A Rust expert would say, *"The testing
and docs are exemplary; the architecture is a wise fidelity tradeoff, but it is
not idiomatic Rust, and the choices that serve that tradeoff (dynamic-`Value`
I/O, flat `pub`, god-modules, config god-object, inline SQL) are the ones that
will bite at the next stage of growth."*

**The two highest-leverage improvements** (both cheap, both address the biggest
structural costs):
1. **Encapsulation pass** — introduce an internal/`pub(crate)` boundary so the
   crate has a seam; shrink the public surface to what the binary + tests
   actually need.
2. **`Config::Default` + a test constructor** — collapse the ~70 giant config
   literals and make the 41-field struct safe to extend.

Everything else (module decomposition, a data-access layer, dependency dedup,
the `unwrap` sweep, ~~the 2024-edition bump~~ (done — §4.10), doc-drift
cleanup) is incremental.

---

## 7. Research sources

- **Rust API Guidelines** — rust-lang.github.io/api-guidelines (public-API
  hygiene, naming, conversions; the "keep the public surface small" principle).
- **Rust editions / current stable** — confirmed stable `1.98.1` (Sept 2026)
  on the 2024 edition; 2024 edition is the default since Rust 1.85; no 2026
  edition. (releases.rs; blog.rust-lang.org; rustfaq.org 2024-edition overview.)
- **Async-trait object safety** — rust-lang async-fundamentals initiative
  (`dyn_async_trait`), users.rust-lang.org, uditsamani.com/async-traits:
  `async fn in traits` is not object-safe; `Pin<Box<dyn Future>>` is the
  standard `dyn`-safe workaround (validates the plugin-host design).
- **`hexuria/impeccable-rust`** — curated "what idiomatic Rust looks like" rules,
  used as the idiomaticity cross-check.
- **General software design** (language-agnostic): module boundaries /
  encapsulation, single-responsibility and the cost of central `match`
  dispatch, dynamic typing in a statically typed language, and the
  maintainability cost of config god-objects.
