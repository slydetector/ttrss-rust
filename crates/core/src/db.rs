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
//! We apply the same statements in sqlx's `before_acquire` hook so every pooled
//! connection has identical session settings (in particular `TIME ZONE 0`,
//! which fixes timestamp interpretation to UTC exactly as PHP does). The
//! statements are idempotent, so re-applying them on each checkout is a no-op.

use anyhow::Result;
use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnection, PgPool};

use crate::config::Config;

/// The exact per-connection session statements from `Db::pdo_connect()`.
const SESSION_SETUP: &[&str] = &[
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
    /// Open a connection pool, applying the PHP session setup to each
    /// connection on acquisition.
    pub async fn connect(cfg: &Config) -> Result<Self> {
        let options = PoolOptions::<sqlx::Postgres>::new()
            .max_connections(10)
            .before_acquire(|conn: &mut PgConnection, _meta| {
                Box::pin(async move {
                    for stmt in SESSION_SETUP {
                        sqlx::query(stmt).execute(&mut *conn).await?;
                    }
                    Ok(true)
                })
            });

        let pool = options.connect(&cfg.db_url()).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;

    /// Connect to the local golden DB (the oracle's postgres, exposed on
    /// 127.0.0.1:5432) and confirm the session setup took effect. Skipped if
    /// the DB isn't reachable.
    #[tokio::test]
    async fn connect_and_session_setup() {
        let cfg = Config {
            db_host: "127.0.0.1".into(),
            db_port: 5432,
            db_user: "postgres".into(),
            db_pass: "password".into(),
            db_name: "postgres".into(),
            db_sslmode: "prefer".into(),
            self_url: "http://127.0.0.1:8280/tt-rss/".into(),
            encryption_key: None,
            listen: "127.0.0.1:8281".parse().unwrap(),
            docroot: PathBuf::from("tt-rss"),
        };

        let db = match Db::connect(&cfg).await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("skipping db test (DB not reachable): {e}");
                return;
            }
        };

        // `TIME ZONE 0` should report UTC+00.
        let row: (i64,) =
            sqlx::query_as("select extract(timezone from now())::bigint")
                .fetch_one(db.pool())
                .await
                .expect("timezone query");
        assert_eq!(row.0, 0, "session timezone should be UTC+0");

        // A trivial read against a real table.
        let count: (i64,) =
            sqlx::query_as("select count(*) from ttrss_feeds")
                .fetch_one(db.pool())
                .await
                .expect("count query");
        assert!(count.0 >= 1, "golden DB should have feeds");
    }
}
