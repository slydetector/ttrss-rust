# Plan: Template Database for Parallel Test Speedup

## Goal

Replace the per-test `CREATE DATABASE` + SQL replay (~350ms/test) with
`CREATE DATABASE ... TEMPLATE` (~20-50ms/test) so that parallel test
binaries (via `cargo-nextest`) each get an isolated DB quickly, with a
shared template built once under an advisory lock.

## Current State

- **585** test functions across **59** files use `#[sqlx::test(fixtures(...))]`
- Each test: `CREATE DATABASE _sqlx_test_N` → replay `schema.sql` + `baseline.sql` + `seed.sql` → run → `DROP DATABASE`
- `#[ctor::ctor]` in each binary calls `test_db::init_database_url()` which starts/attaches the Postgres container and exports `DATABASE_URL`
- `common::make_db(pool, options)` creates the `Db` handle with `statement_cache_capacity(0)` and the `SESSION_SETUP` hook

## Design

### New: `test-db` crate additions

```
crates/test-db/src/
├── lib.rs          (existing: PgServer, init_database_url, ensure_test_pg)
└── template.rs     (NEW: template setup + with_test_db helper)
```

### `template.rs` — two public functions

#### 1. `ensure_template()` — called from `#[ctor]` (once per binary)

```rust
pub async fn ensure_template(master_url: &str) -> Result<()> {
    // 1. Connect to the `postgres` maintenance DB (master_url)
    // 2. SELECT pg_advisory_lock(42) — fixed key, serializes template setup
    // 3. Check: does `_ttrss_tpl` exist AND have a `_ttrss_tpl_meta` table
    //    with a row whose `hash` matches the current schema+seed hash?
    //    - Yes → unlock, return
    //    - No  → DROP (if exists), CREATE DATABASE _ttrss_tpl,
    //            connect to it, apply schema.sql + baseline.sql + seed.sql,
    //            create _ttrss_tpl_meta table, INSERT the hash
    // 4. SELECT pg_advisory_unlock(42)
}
```

**Hash computation:** SHA-256 of the concatenated contents of `schema.sql` +
`baseline.sql` + `seed.sql`. Computed at compile time via `include_str!` and
hashed in the `#[ctor]` (cheap, ~1ms). If the hash doesn't match the stored
marker, the template is rebuilt.

**Advisory lock key:** `42` (arbitrary fixed integer). Only held during
template setup (~1-2s on first run, ~0ms on subsequent runs). Released
before any test starts.

#### 2. `with_test_db` — replaces the `#[sqlx::test]` macro

Since we can't easily write a proc-macro (scope of this task), the helper
is a **free function** that each test calls:

```rust
pub async fn with_test_db<F, T>(master_url: &str, test_fn: F) -> Result<T>
where
    F: FnOnce(PgPool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send>>,
{
    // 1. Connect to master (postgres) DB
    // 2. CREATE DATABASE _ttrss_t_<pid>_<random> TEMPLATE _ttrss_tpl
    // 3. Create pool to the new DB (with statement_cache_capacity(0) + SESSION_SETUP)
    // 4. Call test_fn(pool)
    // 5. DROP DATABASE (in a finally block, even on test failure)
    // 6. Return the test's result
}
```

**DB naming:** `_ttrss_t_{pid}_{rand}` — unique per process, no collision
between parallel binaries. Random suffix handles re-runs within the same
process (e.g., multiple tests in one binary).

**Error behavior:** If the test panics or returns Err, the DB is still
dropped (via a drop guard or explicit catch). For debugging, a
`TTRSS_KEEP_TEST_DB=1` env var can skip the DROP (print the DB name).

### Changes to `#[ctor]` in each test binary

Current:
```rust
#[ctor::ctor(unsafe)]
fn init_test_pg() {
    test_db::init_database_url();
}
```

New:
```rust
#[ctor::ctor(unsafe)]
fn init_test_pg() {
    test_db::init_database_url();
    // Build the template (advisory-locked, ~0ms if already built)
    let url = std::env::var("DATABASE_URL").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(
        test_db::template::ensure_template(&url)
    ).expect("template setup");
}
```

Wait — `#[ctor]` runs before `main`, so we can't use async directly. Two options:
- **A:** Use `tokio::runtime::Runtime::new().block_on(...)` in the ctor (works, but creates a throwaway runtime)
- **B:** Make `ensure_template` synchronous (use a blocking connection via `sqlx`'s sync API or a small dedicated runtime)

**Decision: Option A.** The ctor already runs before the test harness; a
throwaway `Runtime` is fine (it's used once, then dropped). The existing
`init_database_url()` already does blocking I/O (container startup).

### Changes to test functions

**Before** (current):
```rust
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_get_version(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    // ...
}
```

**After:**
```rust
#[tokio::test]
async fn test_get_version() {
    test_db::template::with_test_db(&test_db::master_url(), |pool| Box::pin(async move {
        let (state, sess) = setup_from_pool(pool).await;
        // ...
        Ok(())
    })).await.unwrap();
}
```

**Problem:** This is a **massive diff** — 585 test functions × signature change +
body restructure. The `setup(pool, options)` helper also changes (it takes
`PoolOptions + ConnectOptions` from the macro; now it takes a `PgPool`).

**Mitigation:** Write a **migration script** (sed/python) that does the mechanical
transformation:
1. Replace `#[sqlx::test(fixtures(...))]` → `#[tokio::test]`
2. Change function signature from `(pool: PoolOptions<Postgres>, options: PgConnectOptions)` → `()`
3. Wrap the body in `with_test_db(url, |pool| Box::pin(async move { ... })).await.unwrap()`
4. Replace `setup(pool, options)` → `setup_from_pool(pool)` (new helper)

This is a **one-shot refactor** — after the script runs, no test ever touches
the macro again.

### New helper in `common/mod.rs`

```rust
/// Create a Db from an already-connected pool (replaces make_db).
pub fn make_db_from_pool(pool: PgPool) -> Db {
    Db::from_pool(pool)
}

/// Standard test setup: create AppState + Sess from a pool.
pub async fn setup_from_pool(pool: PgPool) -> (AppState, Sess) {
    let db = make_db_from_pool(pool);
    // ... same as before but using the pool directly
}
```

## Phases

| Phase | Work | Est. time |
|-------|------|-----------|
| 1 | Implement `template.rs` (ensure_template + with_test_db) | 1-2 hrs |
| 2 | Update `#[ctor]` + migrate **2 pilot files** (e.g. `api_writes.rs` + `feed_crud.rs`) | 1 hr |
| 3 | **Verify:** `cargo check` + `cargo test` on pilot files + full suite still green | 30 min |
| 4 | Write migration script + apply to remaining 57 files / 560+ tests | 2-3 hrs |
| 5 | Run full test suite, fix failures | 1-2 hrs |
| 6 | Measure: compare `cargo-nextest run` wall time before/after | 15 min |

**Total: ~1 day** (dominated by Phase 4 script + fixups)

### Pilot files (Phase 2)

Pick two files that represent the common patterns:
- `crates/ttrss/tests/api_writes.rs` — uses `setup(pool, options)` helper
- `crates/ttrss/tests/feed_crud.rs` — uses `make_db(pool, options)` directly

If both migrate cleanly, the script for the rest is low-risk.

## Risks / Open Questions

| Item | Risk | Mitigation |
|------|------|------------|
| `#[ctor]` + `block_on` | Thread safety: `#[ctor]` runs on the main thread before the test harness. `block_on` is fine there. | Verify with a quick test. |
| `CREATE DATABASE` outside transaction | Postgres requires autocommit for DDL. Our `with_test_db` connects, runs DDL (no explicit BEGIN), then creates the pool. | Use a dedicated connection for DDL, separate from the pool. |
| Template DB grows stale | If schema/seed files change, the hash changes → rebuild. | Hash is computed at runtime from `include_str!` — always fresh. |
| 585 test signature changes | Large diff, but mechanical. | Script + `cargo check` + `cargo test` verification. |
| `Box::pin` overhead | Each test wraps its body in a `Box::pin` (one heap alloc). | Negligible vs. the DB setup savings. |
| Some tests use `PoolOptions` directly (not via `setup`) | Need to check if any test uses the pool options for something other than creating the pool. | Grep for `pool` usage outside `setup()` calls. |

## Decisions to Make

1. **Helper style:** `with_test_db(url, |pool| ...)` (closure) vs. a macro
   `#[ttrss_test]` (proc-macro, cleaner call sites but new crate). Recommend:
   **closure** (no proc-macro crate, simpler, can always migrate to macro later).

2. **DB naming:** `_ttrss_t_{pid}_{rand}` vs. `_ttrss_t_{test_fn_name}`.
   Recommend: **pid+rand** (simpler, no need to pass test name).

3. **Keep `#[sqlx::test]` for non-DB tests?** We have a few tests that use
   `#[sqlx::test]` without DB fixtures (e.g., `test-db/tests/smoke.rs`).
   Recommend: **migrate everything** to the new helper for consistency.

4. **`TTRSS_KEEP_TEST_DB` escape hatch:** Print the DB name on failure so
   you can `psql` into it. Recommend: **yes** (one-line env check).
