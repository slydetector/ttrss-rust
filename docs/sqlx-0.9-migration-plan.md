# SQLx 0.7.4 → 0.9.0 Migration Plan

## Overview

Upgrade from sqlx 0.7.4 to 0.9.0. The project moved to `transact-rs/sqlx` but
crates.io still publishes under `launchbadge/sqlx`. The one substantive breaking
change for us is **`SqlSafeStr`**: every query call now requires the SQL string
to be either a `&'static str` literal or an explicitly-wrapped dynamic string.

## Current State

```toml
# Cargo.toml (workspace)
sqlx = { version = "0.7", default-features = false, features = [
    "runtime-tokio-rustls", "postgres", "chrono", "migrate", "macros"
] }
```

Usage:
- **324** total `sqlx::query*` call sites (src + tests)
- **~47** use dynamic strings (variable or `format!`) → need `AssertSqlSafe`
- **~277** use `&'static str` literals (some multi-line) → compile unchanged
- No `query!` macros used → can drop `macros` feature
- No `sqlx::migrate` calls → can drop `migrate` feature
- `statement_cache_capacity(0)` workaround remains (same bug in 0.9)

## Phase 0: Feature Flag Changes (Cargo.toml)

| Feature (0.7) | Feature (0.9) | Notes |
|---|---|---|
| `runtime-tokio-rustls` | `runtime-tokio` + `tls-rustls` | Split into separate runtime + TLS |
| `postgres` | `postgres` | Unchanged |
| `chrono` | `chrono` | Unchanged |
| `migrate` | *(drop)* | Not used in code |
| `macros` | *(drop)* | No `query!` macros in code |

**Result:**
```toml
sqlx = { version = "0.9", default-features = false, features = [
    "runtime-tokio", "tls-rustls", "postgres", "chrono"
] }
```

**Verification:** `cargo check` — expect compile errors only from `SqlSafeStr`.

## Phase 1: `AssertSqlSafe` Wrapping

The one breaking API change. In 0.9, `query()`/`query_as()`/`query_scalar()`
accept a `SqlSafeStr` parameter. `&'static str` implements it directly.
Dynamic strings (owned `String`, `format!`, variables) must be wrapped:

```rust
// Top of each file that has dynamic SQL:
use sqlx::AssertSqlSafe;

// At each dynamic site:
let rows = sqlx::query(AssertSqlSafe(&sql)).fetch_all(pool).await?;
let row = sqlx::query(AssertSqlSafe(format!("SELECT ... WHERE {clause}"))).fetch_one(pool).await?;
```

`AssertSqlSafe` is a transparent newtype — zero cost, compile-time marker.

### Sites to change (47 total)

| File | Count | Pattern |
|---|---|---|
| `src/updates.rs` | 4 | `&rule_q`, `&sql` (2×) |
| `src/scheduler.rs` | 4 | `&sql`, `&format!(...)` (3×) |
| `src/users.rs` | 1 | `&format!(...)` |
| `src/headlines.rs` | 3 | `&query` (3×) |
| `src/system.rs` | 2 | `&format!(...)` (2×) |
| `src/filters.rs` | 1 | `&sql` |
| `src/search.rs` | 1 | `&sql` |
| `src/digest.rs` | 1 | `&format!(...)` |
| `src/feeds.rs` | 8 | `&sql`, `&format!(...)` (5×), `&q` |
| `src/article.rs` | 7 | `&sql` (7×) |
| `src/labels.rs` | 1 | `&sql` |
| `src/api.rs` | 2 | `&q`, `&sql` |
| `src/counters.rs` | 5 | `&q` (4×), `&format!(...)` |
| `src/core/db.rs` | ? | (check) |
| `tests/` | 4 | `&format!(...)` (2×), `&sql` (2×) |
| `test-db/tests/smoke.rs` | 1 | `&format!(...)` |

### Audit note (important)

Each `AssertSqlSafe` wrapping is an **implicit security assertion**. For every
site, the developer confirms:
1. The dynamic portion is a **structural element** (column name, clause,
   table name) — not user input
2. Or the dynamic portion is **sanitized** before interpolation

All 47 sites are currently safe (they build SQL from internal state,
not user input). But the wrap is the formal attestation. Flag any site
where the interpolated value comes from user input for review.

### Pattern for variable-bound SQL

For sites that build a full SQL string into a local `let sql = ...;` and then
call `sqlx::query(&sql)`, the change is mechanical:

```rust
// Before:
let sql = format!("SELECT {} FROM t WHERE {} = $1", col, cond);
let rows = sqlx::query(&sql).bind(val).fetch_all(pool).await?;

// After:
let sql = format!("SELECT {} FROM t WHERE {} = $1", col, cond);
let rows = sqlx::query(AssertSqlSafe(&sql)).bind(val).fetch_all(pool).await?;
```

For `format!` inline:

```rust
// Before:
let rows = sqlx::query(&format!("SELECT ... WHERE uid = $1 AND {scope}"))
    .bind(uid).fetch_all(pool).await?;

// After:
let rows = sqlx::query(AssertSqlSafe(format!("SELECT ... WHERE uid = $1 AND {scope}")))
    .bind(uid).fetch_all(pool).await?;
```

## Phase 2: Statement Cache Workaround

The `statement_cache_capacity(0)` workaround in `core/db.rs` and `test-db`
carries over unchanged. The bug is still present in 0.9 (no fix yet).
No code change needed, just confirm it still compiles.

```rust
// Already in place — no change:
PgPoolOptions::new()
    .statement_cache_capacity(0)
    ...
```

## Phase 3: Cargo.lock + Verification

1. `cargo update -p sqlx` (or edit Cargo.toml and `cargo update`)
2. `cargo check` — fix all `SqlSafeStr` errors (Phase 1)
3. `cargo clippy` — ensure no new lints
4. `cargo test --lib` — unit tests pass
5. `cargo test` — full integration test suite passes

## Phase 4: Cleanup (optional)

- Drop unused features (`macros`, `migrate`) — already done in Phase 0
- Consider removing `chrono` if we ever move to `time` (not now)
- `AssertSqlSafe` import: `use sqlx::AssertSqlSafe;` at top of each file that uses it.
  (Style B — decided)

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Other breaking API changes beyond `SqlSafeStr` | Low | Compile errors will surface them; fix as encountered |
| `chrono` feature behavior change | Low | We use `chrono::NaiveDateTime` for bind/decode; API stable across 0.7→0.9 |
| `statement_cache_capacity(0)` API removed | Very low | Still present in 0.9 (confirmed from source) |
| Dependency ecosystem not yet updated to 0.9 | Medium | Check if any transitive dep still pins sqlx 0.7; `cargo update` will handle |
| `query_builder` behavior change | Low | We use it in a few places; API is stable |

## Effort Estimate

| Phase | Time | Notes |
|---|---|---|
| 0 (Cargo.toml) | 5 min | Trivial |
| 1 (AssertSqlSafe) | 30-60 min | 47 sites, mechanical but needs audit attention |
| 2 (workaround) | 5 min | Verify only |
| 3 (verification) | 15 min | Run tests |
| 4 (cleanup) | 5 min | Import style decision |
| **Total** | **~1-1.5 hrs** | |

## Decisions to Make

1. **Import style for `AssertSqlSafe`:** decided — top-level `use sqlx::AssertSqlSafe;` per file.
   top-level `use sqlx::AssertSqlSafe;` per file. Recommend inline (explicit).
2. **Drop `migrate` and `macros` features:** confirmed not used, but do we want
   them for future-proofing? Recommend: drop (add back when needed).
3. **Branch strategy:** single commit per phase? Or one big commit? Recommend:
3. **Branch strategy:** single commit with the whole migration.
