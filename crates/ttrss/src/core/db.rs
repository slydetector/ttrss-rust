//! PostgreSQL access via [`sqlx`], replicating `classes/Db.php`.
//!
//! `Db::pdo_connect()` runs four `SET` statements on every new connection:
//!
//! ```sql
//! set client_encoding = 'UTF-8';
//! set datestyle = 'ISO, european';
//! set TIME ZONE 0;
//! set cpu_tuple_cost = 0.5;
//! ```
//!
//! We apply the same statements in sqlx's `after_connect` hook so every pooled
//! connection has identical session settings (in particular `TIME ZONE 0`,
//! which fixes timestamp interpretation to UTC exactly as PHP does). The pool
//! never resets session GUCs on return (the idle check is a `ping`, not a
//! `DISCARD`), so setting them once per new connection is enough; re-running
//! them on every acquire added four redundant round-trips per query.

use anyhow::Result;
use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool};
use sqlx::AssertSqlSafe;
use std::str::FromStr;

use crate::core::config::Config;

/// The exact per-connection session statements from `Db::pdo_connect()`.
///
/// Exposed so the DB-backed test suites can apply the identical session
/// settings to their pools (via `before_acquire`), matching the server and
/// the PHP reference exactly.
pub const SESSION_SETUP: &[&str] = &[
    "set client_encoding = 'UTF-8'",
    "set datestyle = 'ISO, european'",
    "set TIME ZONE 0",
    "set cpu_tuple_cost = 0.5",
];

/// A configured Postgres pool.
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Open a connection pool, applying the PHP session setup to each new
    /// connection.
    pub async fn connect(cfg: &Config) -> Result<Self> {
        Self::connect_pool(&cfg.db_url(), 10).await // 10 = the sqlx default the removed `db_max_connections` knob defaulted to
    }

    /// Open a connection pool from an explicit URL, applying the same PHP
    /// session setup as [`Db::connect`] (used by the DB-backed test suites).
    pub async fn connect_url(url: &str) -> Result<Self> {
        Self::connect_pool(url, 10).await
    }

    /// Open a pool from an explicit URL with an explicit `max_connections`, applying
    /// the same PHP session setup as [`Db::connect`].
    pub async fn connect_pool(url: &str, max_connections: u32) -> Result<Self> {
        let options = PgConnectOptions::from_str(url)?
            // sqlx 0.7 keeps a per-connection statement cache keyed by SQL
            // text (default capacity 100): re-Parsing the same text with
            // different parameter type OIDs silently reuses the old
            // prepared statement and produces wire-protocol errors (08P01)
            // instead of a clean type error. PHP's PDO prepares per call
            // and has no such cross-call reuse, so disable the cache.
            .statement_cache_capacity(0);

        let pool = PoolOptions::<sqlx::Postgres>::new()
            .max_connections(max_connections)
            // `after_connect` runs once per *new* connection (not on every
            // checkout). The four statements are session GUCs that persist for
            // the connection's life — sqlx's pool only `ping`s idle connections
            // on return (no `DISCARD ALL`) — so applying them at connect keeps
            // every connection identically configured while dropping four
            // redundant round-trips from every acquire (the headlines view
            // issues dozens of queries, so this was the dominant DB overhead).
            .after_connect(|conn: &mut PgConnection, _meta| {
                Box::pin(async move {
                    for stmt in SESSION_SETUP {
                        sqlx::query(AssertSqlSafe(*stmt)).execute(&mut *conn).await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Wrap an already-connected pool (e.g. one created by a test harness)
    /// in a [`Db`]. Note: the session setup is applied at connect time, so a
    /// pool created elsewhere does not automatically carry it — apply
    /// [`SESSION_SETUP`] on acquisition if it needs to match the server.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}
