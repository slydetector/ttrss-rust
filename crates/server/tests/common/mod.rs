//! Shared setup for the DB-backed parity test binaries (the test-parity
//! program in `docs/plan.md`, rows 8–14).
//!
//! The `#[ctor]` function (which runs before the test harness's `main`)
//! starts/attaches to the dedicated test Postgres and exports `DATABASE_URL`;
//! sqlx's `#[sqlx::test]` harness then creates a fresh `_sqlx_test_N`
//! database per test, applies the fixture files listed on each test (the
//! frozen reference's base schema + baseline + the PHP integration suite's
//! `seed.sql`) in order, injects the pool options, and drops the database
//! when the test succeeds.
//!
//! [`make_db`] wraps the injected options in a core [`Db`] with the server's
//! exact per-connection session setup (`ttrss_core::db::SESSION_SETUP`:
//! `TIME ZONE 0`, `datestyle = 'ISO, european'`, …) so test connections
//! behave exactly like the server's and the PHP reference's.

use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgConnection, Postgres};
use ttrss_core::db::{Db, SESSION_SETUP};

#[ctor::ctor(unsafe)]
fn init_test_pg() {
    ttrss_testing::init_database_url();
}

/// Wrap the pool options injected by `#[sqlx::test]` in a core [`Db`],
/// applying the PHP per-connection session setup to every acquired
/// connection.
pub async fn make_db(
    pool: PoolOptions<Postgres>,
    options: PgConnectOptions,
) -> anyhow::Result<Db> {
    let pool = pool
        .before_acquire(|conn: &mut PgConnection, _meta| {
            Box::pin(async move {
                for stmt in SESSION_SETUP {
                    sqlx::query(stmt).execute(&mut *conn).await?;
                }
                Ok(true)
            })
        })
        .connect_with(options)
        .await?;
    Ok(Db::from_pool(pool))
}
