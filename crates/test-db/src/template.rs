//! Template-database test isolation: `CREATE DATABASE ... TEMPLATE` for
//! fast per-test DB creation (~20-50ms vs ~350ms of SQL replay).
//!
//! ## How it works
//!
//! 1. [`ensure_template`] (called once per test binary from `#[ctor]`)
//!    builds a shared template database (`_ttrss_tpl`) under a Postgres
//!    advisory lock. The template contains the full schema + baseline + seed
//!    fixtures. A SHA-256 marker ensures the template is rebuilt when the
//!    fixture files change.
//!
//! 2. [`with_test_db`] (called per test) clones the template into a unique
//!    database (`_ttrss_t_{pid}_{n}`), hands the test the database URL,
//!    then drops the database when the test finishes (success or failure).
//!
//! ## Concurrency
//!
//! Multiple test binaries can run in parallel (e.g. via `cargo-nextest`).
//! The advisory lock serializes the one-time template setup; the per-test
//! clones are lock-free and fully parallel.

use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest as _, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, Connection as _, PgConnection, PgPool};
use url::Url;

/// The fixed name of the shared template database.
pub const TEMPLATE_DB: &str = "_ttrss_tpl";

/// Advisory lock key for template setup (arbitrary fixed integer).
const ADVISORY_LOCK_KEY: i64 = 42;

/// The fixture parts, concatenated and hashed to detect staleness. The base
/// schema and the seed are the frozen reference's own files (submodule pinned
/// by the superproject — the schema is the same file `core::schema` runs at
/// runtime; the seed is the one the PHP integration suite runs — so the
/// tests track the reference pin; a local copy would drift silently when the
/// pin moves). `baseline.sql` carries what the app bootstrap adds on top of
/// the schema (the `ttrss_version` stamp + the `pg_trgm` extension).
const FIXTURES: &[&str] = &[
    include_str!("../../../tt-rss/sql/pgsql/schema.sql"),
    include_str!("../fixtures/baseline.sql"),
    include_str!("../../../tt-rss/tests_integration/seed.sql"),
    SEED_TSVECTOR_BACKFILL,
];

/// Rust-test-only delta appended to the reference seed: backfill
/// `tsvector_combined` on the seeded entries. tt-rss maintains the column in
/// its import/update paths (RSSUtils.php / Article.php), not via a trigger,
/// so the reference seed omits it; the search parity tests query it directly
/// on the seeded rows.
const SEED_TSVECTOR_BACKFILL: &str = "\
-- Rust-test-only: backfill the full-text column (tt-rss maintains it in its
-- import/update paths, not via a trigger, so the reference seed omits it).
UPDATE ttrss_entries
SET tsvector_combined = to_tsvector(COALESCE(lang, 'english')::regconfig, title || ' ' || content);
";

/// Return the master (maintenance) database URL from `DATABASE_URL`.
pub fn master_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL not set (ctor should have set it)")
}

/// Compute the SHA-256 of the concatenated fixture files.
fn fixture_hash() -> String {
    let mut hasher = Sha256::new();
    for f in FIXTURES {
        hasher.update(f.as_bytes());
        hasher.update(b"\n--\n");
    }
    hex::encode(hasher.finalize())
}

/// Replace the database name in a Postgres URL.
fn with_db(url: &str, db: &str) -> String {
    let mut u: Url = url.parse().unwrap();
    u.set_path(&format!("/{db}"));
    u.to_string()
}

/// Parse a Postgres URL into `PgConnectOptions`.
fn parse_pg_opts(url: &str) -> Result<PgConnectOptions> {
    let u: Url = url.parse().context("parse URL")?;
    let mut opts = PgConnectOptions::new();
    if let Some(host) = u.host_str() {
        opts = opts.host(host);
    }
    if let Some(port) = u.port() {
        opts = opts.port(port);
    }
    let user = u.username();
    if !user.is_empty() {
        opts = opts.username(user);
    }
    if let Some(pwd) = u.password() {
        opts = opts.password(pwd);
    }
    let db = u.path().trim_start_matches('/');
    if !db.is_empty() {
        opts = opts.database(db);
    }
    Ok(opts)
}

/// Ensure the shared template database exists and is up-to-date.
///
/// Called once per test binary from `#[ctor]`. Safe to call concurrently
/// from multiple processes: the advisory lock serializes the setup phase.
pub async fn ensure_template(url: &str) -> Result<()> {
    let mut conn = connect(url).await?;

    // Serialize template setup across concurrent processes.
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut conn)
        .await
        .context("acquire advisory lock")?;

    let result = check_or_build_template(&mut conn, url).await;

    // Release the lock.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut conn)
        .await;

    let _ = conn.close().await;
    result
}

async fn check_or_build_template(conn: &mut PgConnection, url: &str) -> Result<()> {
    let expected_hash = fixture_hash();

    // Check if the template DB exists.
    let exists: Option<bool> =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(TEMPLATE_DB)
            .fetch_one(&mut *conn)
            .await
            .context("check template existence")?;

    if !exists.unwrap_or(false) {
        build_template(conn, &expected_hash, url).await?;
        return Ok(());
    }

    // Template exists — check the hash marker (must connect to the template DB;
    // Postgres does not support cross-database queries).
    let tpl_url = with_db(url, TEMPLATE_DB);
    let stored_hash = match connect(&tpl_url).await {
        Ok(mut tpl_conn) => {
            let h: Option<String> = sqlx::query_scalar("SELECT hash FROM _ttrss_tpl_meta LIMIT 1")
                .fetch_optional(&mut tpl_conn)
                .await
                .unwrap_or(None);
            let _ = tpl_conn.close().await;
            h
        }
        Err(_) => None,
    };

    if stored_hash.as_deref() == Some(expected_hash.as_str()) {
        return Ok(());
    }

    eprintln!("[test-db] template {TEMPLATE_DB} is stale, rebuilding…");
    drop_template_db(conn).await?;
    build_template(conn, &expected_hash, url).await
}

async fn build_template(conn: &mut PgConnection, hash: &str, url: &str) -> Result<()> {
    eprintln!("[test-db] building template database {TEMPLATE_DB}…");
    let start = std::time::Instant::now();

    sqlx::query("CREATE DATABASE _ttrss_tpl")
        .execute(&mut *conn)
        .await
        .context("CREATE DATABASE template")?;

    // Connect to the template DB and apply fixtures.
    let tpl_url = with_db(url, TEMPLATE_DB);
    let mut tpl_conn = connect(&tpl_url).await.context("connect to template")?;

    for fixture in FIXTURES {
        sqlx::raw_sql(AssertSqlSafe(fixture.to_string()))
            .execute(&mut tpl_conn)
            .await
            .context("apply fixture to template")?;
    }

    // Insert the hash marker.
    sqlx::query("CREATE TABLE _ttrss_tpl_meta (hash TEXT PRIMARY KEY)")
        .execute(&mut tpl_conn)
        .await
        .context("create template meta table")?;
    sqlx::query("INSERT INTO _ttrss_tpl_meta (hash) VALUES ($1)")
        .bind(hash)
        .execute(&mut tpl_conn)
        .await
        .context("insert template marker")?;

    let _ = tpl_conn.close().await;
    eprintln!(
        "[test-db] template {TEMPLATE_DB} ready in {:?}",
        start.elapsed()
    );
    Ok(())
}

async fn drop_template_db(conn: &mut PgConnection) -> Result<()> {
    let _ = sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '_ttrss_tpl'",
    )
    .execute(&mut *conn)
    .await;

    sqlx::query("DROP DATABASE IF EXISTS _ttrss_tpl")
        .execute(&mut *conn)
        .await
        .context("DROP DATABASE template")?;
    Ok(())
}

/// Clone the template into a unique per-test database, run the test, then drop it.
///
/// The `test_fn` closure receives the **URL** of the new database. The test
/// is responsible for creating its own pool/connection with the appropriate
/// session settings.
///
/// Set `TTRSS_KEEP_TEST_DB=1` to skip the DROP (for debugging); the DB name
/// is printed on failure.
pub async fn with_test_db<F, T>(test_fn: F) -> Result<T>
where
    F: FnOnce(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send>>,
{
    let url = master_url();
    let db_name = unique_db_name();

    let mut conn = connect(&url).await?;

    // Clone the template.
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE {db_name} TEMPLATE {TEMPLATE_DB}"
    )))
    .execute(&mut conn)
    .await
    .context("CREATE DATABASE from template")?;

    // Build the URL for the new DB.
    let db_url = with_db(&url, &db_name);

    // Run the test.
    let result = test_fn(db_url.clone()).await;

    if result.is_err() {
        eprintln!("[test-db] test failed; DB: {db_name}");
    }

    // Drop the test DB.
    if std::env::var("TTRSS_KEEP_TEST_DB").is_err() {
        let _ = sqlx::query(AssertSqlSafe(format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{db_name}'"
        )))
        .execute(&mut conn)
        .await;
        sqlx::query(AssertSqlSafe(format!("DROP DATABASE IF EXISTS {db_name}")))
            .execute(&mut conn)
            .await
            .context("DROP DATABASE test")?;
    } else {
        eprintln!("[test-db] TTRSS_KEEP_TEST_DB set; keeping {db_name}");
    }

    let _ = conn.close().await;
    result
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn unique_db_name() -> String {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("_ttrss_t_{}_{}", std::process::id(), n)
}

async fn connect(url: &str) -> Result<PgConnection> {
    let opts = parse_pg_opts(url)?;
    let conn = PgConnection::connect_with(&opts).await.context("connect")?;
    Ok(conn)
}

/// Create a pooled connection with standard test settings.
pub async fn make_pool(url: &str) -> PgPool {
    let opts = parse_pg_opts(url).unwrap().statement_cache_capacity(0);
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(60))
        .connect_with(opts)
        .await
        .expect("pool connect")
}
