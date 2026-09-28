//! Test support for ttrss: a **dedicated** Postgres server for the
//! DB-backed parity suites (test-parity rows 8–14 in `docs/01-plan.md`).
//!
//! Design (see `docs/01-plan.md`, "Test infrastructure for DB-backed suites"):
//!
//! - [`PgServer`] — a Postgres instance dedicated to tests, fully separate
//!   from the oracle stack:
//!   - `TTRSS_TEST_PG_URL` (env) overrides the server (any dedicated
//!     instance, e.g. a long-lived local one);
//!   - otherwise a testcontainers `postgres:17-alpine` container with a
//!     fixed name ([`CONTAINER_NAME`]) that is *reused* across runs while
//!     it exists (one-off container startup, cheap attach afterwards).
//!     Reused containers are **not** removed when their handle is dropped
//!     (testcontainers 0.27.3: `ContainerAsync::drop` returns early for
//!     `ReuseDirective::Always`/`CurrentSession`), so the instance persists.
//! - Per-test isolation is provided by sqlx's own `#[sqlx::test]` harness,
//!   which reads `DATABASE_URL` at runtime: it creates a fresh
//!   `_sqlx_test_N` database per test, applies the `fixtures(...)` in order
//!   (each file as one command), injects the pool options, and drops the
//!   database when the test succeeds. Stale databases from killed runs are
//!   reaped by the harness itself on the next run (the `_sqlx_test.databases`
//!   bookkeeping table + a 2 s margin).
//! - [`init_database_url`] is the `#[ctor::ctor]` entry point test binaries
//!   use to start/attach the server and export `DATABASE_URL` before the
//!   test harness runs.
//!
//! The versioned fixture files (`fixtures/schema.sql`, `baseline.sql`,
//! `seed.sql` next to this crate) — the frozen reference's base schema, the
//! baseline (version 151 + admin user), and the PHP integration suite's
//! `seed.sql` — are referenced by **explicit path** from each test file
//! (`include_str!` is source-file-relative). This intentionally does **not**
//! depend on the oracle's golden data dump: the parity suites run under the
//! same conditions as the PHP reference's own integration tests.

pub mod template;
pub use template::master_url;

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::core::Mount;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt, ReuseDirective};

/// Fixed name of the dedicated test Postgres container.
pub const CONTAINER_NAME: &str = "ttrss_test_pg";

// ---------------------------------------------------------------------------
// Dedicated Postgres server
// ---------------------------------------------------------------------------

/// A handle to the dedicated test Postgres server.
///
/// The optional container handle keeps a testcontainers-managed container
/// alive for the duration of the process; reusable containers are not removed
/// on drop, so a long-lived dedicated instance persists across runs.
pub struct PgServer {
    pub url: String,
    #[allow(dead_code)]
    container: Option<ContainerAsync<Postgres>>,
}

impl PgServer {
    /// Acquire (or start, then attach to) the dedicated test server.
    pub async fn acquire() -> Result<Self> {
        if let Ok(url) = std::env::var("TTRSS_TEST_PG_URL") {
            let url = url.trim().to_string();
            wait_ready(&url, Duration::from_secs(10)).await?;
            return Ok(Self {
                url,
                container: None,
            });
        }

        // Start (or attach to the existing reusable container). Concurrent
        // starters can race on the container name; retry — a later attempt
        // finds the container created by the winner.
        let mut attempts = 0;
        loop {
            attempts += 1;
            let request = Postgres::default()
                .with_tag("17-alpine")
                .with_container_name(CONTAINER_NAME)
                .with_reuse(ReuseDirective::Always)
                .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"));
            match request.start().await {
                Ok(container) => {
                    let port = container
                        .get_host_port_ipv4(5432)
                        .await
                        .context("test Postgres port")?;
                    let host = match container.get_host().await.context("test Postgres host")? {
                        url::Host::Domain(d) => d,
                        url::Host::Ipv4(ip) => ip.to_string(),
                        url::Host::Ipv6(ip) => format!("[{ip}]"),
                    };
                    let url = format!(
                        "postgres://postgres:postgres@{host}:{port}/postgres?sslmode=disable"
                    );
                    // Covers attaching to a container that is not ready yet
                    // (started concurrently by another test process).
                    wait_ready(&url, Duration::from_secs(60)).await?;
                    return Ok(Self {
                        url,
                        container: Some(container),
                    });
                }
                Err(e) if attempts < 30 => {
                    // Name collision / transient Docker error: back off and
                    // retry; the next attempt attaches to the existing box.
                    eprintln!("[test-db] container start attempt {attempts} failed: {e}; retrying");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    return Err(anyhow!("failed to start dedicated test Postgres: {e}"));
                }
            }
        }
    }
}

/// Connect until `SELECT 1` succeeds or the timeout elapses.
async fn wait_ready(url: &str, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(url)
            .await
        {
            Ok(pool) => {
                pool.close().await;
                return Ok(());
            }
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => {
                return Err(anyhow!("test Postgres at {url} not ready: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test-binary entry point
// ---------------------------------------------------------------------------

/// Ensure the dedicated test Postgres is up and return its URL (the
/// management database whose `_sqlx_test` schema the harness bookkeeps in).
pub async fn ensure_test_pg() -> Result<String> {
    Ok(PgServer::acquire().await?.url)
}

/// Synchronous entry point for `#[ctor::ctor]` in test binaries: start (or
/// attach to) the dedicated test Postgres and export `DATABASE_URL` so
/// sqlx's `#[sqlx::test]` harness can manage per-test databases.
///
/// No-op when `DATABASE_URL` is already set (full override; the variable is
/// consulted by the harness at runtime). Fails the binary fast if the test
/// server cannot be started.
pub fn init_database_url() {
    if std::env::var_os("DATABASE_URL").is_some() {
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("ctor: build tokio runtime");
    let url = rt
        .block_on(ensure_test_pg())
        .unwrap_or_else(|e| panic!("ctor: dedicated test Postgres unavailable: {e}"));
    eprintln!("[test-db] test Postgres ready: {url}");
    // `std::env::set_var` is `unsafe` (data race with concurrent readers);
    // safe here: `#[ctor]` runs before any other code in the process.
    unsafe { std::env::set_var("DATABASE_URL", url) };
}

// ---------------------------------------------------------------------------
// Seed helpers (minimal rows on top of the fixture state)
// ---------------------------------------------------------------------------

/// Insert a user with a known password hash (mirrors the `INSERT INTO
/// ttrss_users` setup of the PHP integration tests). Returns the new id.
pub async fn seed_user(
    pool: &PgPool,
    login: &str,
    pwd_hash: &str,
    salt: &str,
    access_level: i32,
) -> Result<i32> {
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(login)
    .bind(pwd_hash)
    .bind(salt)
    .bind(access_level)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Compute an SSHA-512 `pwd_hash` string (`SSHA-512:<hex>`) for a
/// `login`-derived fixed salt — deterministic per login, mirroring
/// `UserHelper::hash_password($pass, $salt, HASH_ALGO_SSHA512)`.
///
/// Returns `(hash, salt)`.
pub fn ssha512_password(login: &str, password: &str) -> (String, String) {
    use sha2::{Digest as _, Sha512};
    let salt = hex::encode(
        &Sha512::new()
            .chain_update(b"ttrss-test-salt:")
            .chain_update(login.as_bytes())
            .finalize()[..16],
    );
    let mut h = Sha512::new();
    h.update(salt.as_bytes());
    h.update(password.as_bytes());
    (format!("SSHA-512:{}", hex::encode(h.finalize())), salt)
}
