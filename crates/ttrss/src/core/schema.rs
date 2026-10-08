//! Schema bootstrap & migration at startup — port of
//! `classes/Db_Migrations.php` (frozen reference 2f7d831).
//!
//! The engine reads the SQL artifacts from the docroot at runtime
//! (`<docroot>/sql/pgsql` — the same tree the app serves static assets
//! from; single source of truth, no embedding) and keeps the version in
//! PHP's own single-row `ttrss_version` table so a DB stays interchangeable
//! with the PHP reference in both directions. See
//! `docs/20-plan-schema-migration.md`.
//!
//! Semantics (PHP parity):
//! - fresh DB (`ttrss_version` absent) → `schema.sql` in one transaction,
//!   version stamped straight to `SCHEMA_VERSION` (`base_is_latest`);
//! - older DB → `migrations/<v>.sql` ascending, one transaction per file
//!   (all statements + the version update commit together);
//! - a failing file rolls back and the version stays at the last applied;
//! - a DB stamped *above* `SCHEMA_VERSION` fails startup (d7 in the plan).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use sqlx::AssertSqlSafe;
use sqlx::postgres::PgPool;
use tracing::{debug, info, warn};

use crate::core::config::Config;
use crate::core::db::Db;

/// `Config::SCHEMA_VERSION` — the latest schema this build knows, as the
/// INT4 width the `ttrss_version` column has.
pub const SCHEMA_VERSION: i32 = crate::core::config::SCHEMA_VERSION as i32;

/// `ttrss_version` — PHP's single-row migrations table (no primary key).
const MIGRATIONS_TABLE: &str = "ttrss_version";

/// Session-level advisory lock serializing concurrent startups' migrations.
/// (Hardening — the PHP reference has no such guard.)
const MIGRATION_LOCK_KEY: i64 = 4289;

/// `Db_Migrations` — the engine. PHP's `initialize($root_path, …)` took the
/// `sql` directory; `from_root` mirrors that.
pub struct Migrations {
    root: PathBuf,
    /// `max_version_override` — the `SCHEMA_VERSION` cap. 0 would fall back
    /// to the highest migration file on disk (PHP's plugin-migration path).
    max_override: i32,
}

impl Migrations {
    /// Production constructor: `<docroot>/sql`, capped at `SCHEMA_VERSION`.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_override: SCHEMA_VERSION,
        }
    }

    /// `base_path` — the `pgsql` directory under the root.
    fn base_path(&self) -> PathBuf {
        self.root.join("pgsql")
    }

    /// `migrations_path` — the numbered migration files.
    fn migrations_path(&self) -> PathBuf {
        self.base_path().join("migrations")
    }

    /// `get_max_version` — the override wins; otherwise the highest
    /// `<int>.sql` filename on disk (PHP: `glob` + `natsort` — numeric
    /// names, so numeric order equals natsort).
    pub async fn get_max_version(&self) -> Result<i32> {
        if self.max_override > 0 {
            return Ok(self.max_override);
        }
        let mut max = 0;
        let rd = match tokio::fs::read_dir(self.migrations_path()).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut entries = rd;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let stem = name.to_string_lossy();
            if let Some(v) = stem
                .strip_suffix(".sql")
                .and_then(|s| s.parse::<i32>().ok())
            {
                max = max.max(v);
            }
        }
        Ok(max)
    }

    /// `get_version` — `-1` when no row exists. The table is created
    /// eagerly (PHP creates it lazily on read failure — same end state,
    /// d5 in the plan).
    pub async fn get_version(&self, pool: &PgPool) -> Result<i32> {
        sqlx::query(AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (schema_version integer not null)"
        )))
        .execute(pool)
        .await?;
        let v: Option<i32> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT schema_version FROM {MIGRATIONS_TABLE} LIMIT 1"
        )))
        .fetch_optional(pool)
        .await?;
        Ok(v.unwrap_or(-1))
    }

    /// `is_migration_needed` — `version != max`.
    pub async fn is_migration_needed(&self, pool: &PgPool) -> Result<bool> {
        Ok(self.get_version(pool).await? != self.get_max_version().await?)
    }

    /// `set_version` — PHP's fetch-then-INSERT-or-UPDATE on the PK-less
    /// single-row table. Runs inside the caller's transaction.
    async fn set_version(&self, version: i32, tx: &mut sqlx::PgConnection) {
        let existing: Option<i32> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT schema_version FROM {MIGRATIONS_TABLE} LIMIT 1"
        )))
        .fetch_optional(&mut *tx)
        .await
        .expect("version read in an open transaction");
        if existing.is_some() {
            sqlx::query(AssertSqlSafe(format!(
                "UPDATE {MIGRATIONS_TABLE} SET schema_version = $1"
            )))
            .bind(version)
            .execute(&mut *tx)
            .await
            .expect("version update in an open transaction");
        } else {
            sqlx::query(AssertSqlSafe(format!(
                "INSERT INTO {MIGRATIONS_TABLE} (schema_version) VALUES ($1)"
            )))
            .bind(version)
            .execute(&mut *tx)
            .await
            .expect("version insert in an open transaction");
        }
    }

    /// `migrate_to` — apply one file in its own transaction. Version `0`
    /// is the base schema file; with `base_is_latest` it stamps the max
    /// version (a fresh install skips the numbered files).
    async fn migrate_to(&self, pool: &PgPool, version: i32) -> Result<()> {
        let path = if version > 0 {
            self.migrations_path().join(format!("{version}.sql"))
        } else {
            self.base_path().join("schema.sql")
        };
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading schema file {}", path.display()))?;
        let statements = split_statements(&text);
        if statements.is_empty() {
            bail!(
                "migration failed: schema file {} is empty or missing",
                path.display()
            );
        }
        if version == 0 {
            info!("schema: loading base database schema");
        } else {
            info!(version, "schema: applying migration");
        }
        let mut tx = pool.begin().await?;
        for statement in &statements {
            debug!(statement, "schema: executing");
            sqlx::query(AssertSqlSafe(statement.clone()))
                .execute(&mut *tx)
                .await?;
        }
        let stamped = if version == 0 {
            // `base_is_latest = true` (PHP's production setting).
            self.get_max_version().await?
        } else {
            version
        };
        self.set_version(stamped, &mut tx).await;
        tx.commit().await?;
        Ok(())
    }

    /// `migrate` — the whole job: base schema on a fresh DB, then the
    /// numbered files ascending. Fail-closed: any error propagates
    /// (the process must not serve a half-migrated schema).
    pub async fn migrate(&self, pool: &PgPool) -> Result<()> {
        let max = self.get_max_version().await?;
        let mut current = self.get_version(pool).await?;
        if current == -1 {
            self.migrate_to(pool, 0).await?;
            current = self.get_version(pool).await?;
        }
        for i in (current + 1)..=max {
            self.migrate_to(pool, i).await?;
        }
        // d7: a DB stamped above the build's ceiling (e.g. run by a newer
        // PHP) must not be served.
        if current > max {
            bail!(
                "schema version {current} is newer than this build (v{max}); \
                 upgrade ttrss before starting"
            );
        }
        info!(version = current, "schema: up to date");
        Ok(())
    }
}

/// Startup entry point (called from `main` before the daemon spawns and the
/// listener binds; any future CLI reuses it):
///
/// 1. take the advisory lock (a concurrent startup may finish the job);
/// 2. `CREATE EXTENSION IF NOT EXISTS pg_trgm` (trusted ext since PG 14;
///    warn-only failure — its only consumer, `af_psql_trgm`, is off by
///    default, d6 in the plan);
/// 3. migrate to `SCHEMA_VERSION`.
pub async fn ensure_up_to_date(config: &Config, db: &Db) -> Result<()> {
    let pool = db.pool();
    // Held until the end of the scope; released when the connection drops.
    let mut lock_conn = pool.acquire().await?;
    sqlx::query(AssertSqlSafe(format!(
        "SELECT pg_advisory_lock({MIGRATION_LOCK_KEY})"
    )))
    .execute(&mut *lock_conn)
    .await?;

    if let Err(e) = sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm")
        .execute(pool)
        .await
    {
        warn!("pg_trgm extension not available: {e} (the af_psql_trgm plugin will be unusable)");
    }

    Migrations::from_root(config.docroot.join("sql"))
        .migrate(pool)
        .await?;
    Ok(())
}

/// `Db_Migrations::get_lines` — split a schema/migration file into the
/// individual statements the engine executes.
///
/// Byte-faithful to the PHP, including its quirks:
/// - the `--` comment check is on the **untrimmed** line (an indented
///   `  -- x` is *not* a comment line and stays in the stream — harmless,
///   Postgres treats `--` as a line comment);
/// - the `begin`/`commit` filter is an **exact** case-insensitive match on
///   the untrimmed statement (a whitespace-padded `  begin` would survive —
///   no real file has one; the count pin in the tests guards that);
/// - the `;` split is naive (no string-literal or `$$` awareness); the 150
///   reference files are verified safe under it (statement-count pin).
pub fn split_statements(text: &str) -> Vec<String> {
    text.split(['\r', '\n'])
        .filter(|l| !l.trim().is_empty() && !l.starts_with("--"))
        .collect::<Vec<_>>()
        .join("")
        .split(';')
        .filter(|s| {
            !s.trim().is_empty() && !matches!(s.to_ascii_lowercase().as_str(), "begin" | "commit")
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── splitter semantics (synthetic input) ─────────────────────────────

    #[test]
    fn line_comment_at_line_start_is_dropped() {
        let out = split_statements("-- a comment\nselect 1;\n");
        assert_eq!(out, vec!["select 1"]);
    }

    #[test]
    fn indented_comment_is_kept_like_php() {
        // PHP checks `str_starts_with($line, "--")` on the untrimmed line —
        // `  -- x` is kept in the stream (Postgres sees it as a line comment
        // inside the statement text).
        let out = split_statements("  -- indented\nselect 1;\n");
        // the PHP joins lines with no separator — the kept line glues on.
        assert_eq!(out, vec!["  -- indentedselect 1"]);
    }

    #[test]
    fn empty_and_blank_lines_are_dropped() {
        let out = split_statements("\n   \n\t\nselect 1;\n\n");
        assert_eq!(out, vec!["select 1"]);
    }

    #[test]
    fn begin_commit_are_removed_case_insensitively() {
        let out = split_statements("begin;\nselect 1;\nCOMMIT;\n");
        assert_eq!(out, vec!["select 1"]);
        let out = split_statements("Begin;\nselect 1;\nCommit;\n");
        assert_eq!(out, vec!["select 1"]);
    }

    #[test]
    fn begin_commit_match_is_exact_not_trimmed() {
        // `  begin` (padded) is NOT in the PHP's ["begin","commit"] list.
        let out = split_statements("  begin\nselect 1;\n");
        // the padded token survives the filter and glues onto the next line.
        assert_eq!(out, vec!["  beginselect 1"]);
    }

    #[test]
    fn multiple_statements_on_one_line() {
        let out = split_statements("select 1;select 2;\n");
        assert_eq!(out, vec!["select 1", "select 2"]);
    }

    #[test]
    fn crlf_and_trailing_semicolon() {
        let out = split_statements("begin;\r\nselect 1;\r\ncommit;\r\n");
        assert_eq!(out, vec!["select 1"]);
    }

    // ── statement-count pin against the real reference files ─────────────

    fn sql_root() -> &'static str {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../tt-rss/sql/pgsql")
    }

    #[test]
    fn schema_sql_statement_count_is_pinned() {
        let text =
            std::fs::read_to_string(format!("{}/schema.sql", sql_root())).expect("read schema.sql");
        assert_eq!(split_statements(&text).len(), 115);
    }

    #[test]
    fn all_migration_files_parse_with_pinned_counts() {
        let root = Path::new(sql_root()).join("migrations");
        let mut files = std::fs::read_dir(&root)
            .expect("read migrations dir")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(files.len(), 149, "expected 149 migration files");

        let mut total = 0;
        for name in &files {
            let text = std::fs::read_to_string(root.join(name)).unwrap();
            let statements = split_statements(&text);
            assert!(!statements.is_empty(), "{name} parses to zero statements");
            total += statements.len();
        }
        assert_eq!(total, 593, "total migration statement count moved");
    }
}
