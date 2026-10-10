//! DB tests for the startup schema bootstrap/migration engine
//! (`ttrss::core::schema`, design in `docs/20-plan-schema-migration.md`).
//!
//! The PHP reference has no `Db_Migrations` tests — these pin the ported
//! semantics: fresh install, no-op at latest, upgrade walk, per-file
//! rollback, and the `SCHEMA_VERSION` ceiling.

mod common;

use sqlx::AssertSqlSafe;
use sqlx::postgres::PgPool;
use test_db::template::with_test_db;
use ttrss::core::db::Db;
use ttrss::core::schema::{Migrations, SCHEMA_VERSION, split_statements};

/// The reference SQL tree (single source of truth, read at test time).
fn sql_root() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tt-rss/sql")
}

/// Build a [`Migrations`] rooted at the reference `tt-rss/sql` tree.
fn ref_migrations() -> Migrations {
    Migrations::from_root(sql_root().to_string())
}

/// Drop everything in the cloned test DB → the "fresh database" state
/// (no `ttrss_version`, no tables).
async fn make_fresh(pool: &PgPool) {
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("CREATE SCHEMA public")
        .execute(pool)
        .await
        .unwrap();
}

/// Apply the base `schema.sql` directly (bypassing the engine) and stamp an
/// arbitrary version — builds a v149-shaped DB for the upgrade tests.
async fn apply_base_schema(pool: &PgPool) {
    let text = std::fs::read_to_string(format!("{}/pgsql/schema.sql", sql_root())).unwrap();
    for stmt in split_statements(&text) {
        sqlx::query(AssertSqlSafe(stmt))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// v149-shaped DB: base schema minus `ttrss_scheduled_tasks` (added by
/// 150.sql), stamped 149.
async fn make_v149(pool: &PgPool) {
    make_fresh(pool).await;
    apply_base_schema(pool).await;
    sqlx::query("DROP TABLE ttrss_scheduled_tasks")
        .execute(pool)
        .await
        .unwrap();
    // The table must exist for the stamp (the engine creates it eagerly; do
    // it explicitly here to match its end state).
    sqlx::query("CREATE TABLE ttrss_version (schema_version integer not null)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ttrss_version (schema_version) VALUES (149)")
        .execute(pool)
        .await
        .unwrap();
}

/// Copy the reference SQL tree into a temp dir, then apply `doctored`.
fn temp_sql_root(doctored: impl Fn(&std::path::Path)) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    // Keep the dir alive for the process (tests drop it at exit).
    std::mem::forget(dir);
    let pgsql = root.join("pgsql");
    std::fs::create_dir_all(pgsql.join("migrations")).unwrap();
    std::fs::copy(
        format!("{}/pgsql/schema.sql", sql_root()),
        pgsql.join("schema.sql"),
    )
    .unwrap();
    let migrations = std::path::Path::new(sql_root()).join("pgsql/migrations");
    for e in std::fs::read_dir(migrations).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), pgsql.join("migrations").join(e.file_name())).unwrap();
    }
    doctored(&pgsql);
    root
}

async fn version_of(pool: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT schema_version FROM ttrss_version LIMIT 1")
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap()
}

/// Fresh database → full schema, stamped straight to v151 (base_is_latest),
/// admin user seeded, exactly one version row.
#[tokio::test]
async fn fresh_db_gets_full_schema_and_latest_version() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            make_fresh(pool).await;

            ref_migrations().migrate(pool).await?;

            assert_eq!(version_of(pool).await, SCHEMA_VERSION);
            let tables: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             AND table_name <> 'ttrss_version'",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(tables, 33, "the reference schema creates 33 tables");
            let admin: Option<String> =
                sqlx::query_scalar("SELECT pwd_hash FROM ttrss_users WHERE login = 'admin'")
                    .fetch_optional(pool)
                    .await?;
            assert_eq!(
                admin.as_deref(),
                Some("SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8"),
                "the stock admin/password seed from schema.sql"
            );
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_version")
                .fetch_one(pool)
                .await?;
            assert_eq!(rows, 1, "ttrss_version holds exactly one row");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// DB already at the latest version → `migrate()` is a no-op.
#[tokio::test]
async fn latest_db_is_a_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            // The template clone is already at v151 with the full schema.
            assert_eq!(version_of(pool).await, SCHEMA_VERSION);

            ref_migrations().migrate(pool).await?;

            assert_eq!(version_of(pool).await, SCHEMA_VERSION);
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_version")
                .fetch_one(pool)
                .await?;
            assert_eq!(rows, 1, "no duplicate version rows on a no-op run");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// v149 DB → 150.sql and 151.sql applied in ascending order → v151.
#[tokio::test]
async fn v149_db_upgrades_through_150_and_151() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            make_v149(pool).await;

            ref_migrations().migrate(pool).await?;

            assert_eq!(version_of(pool).await, SCHEMA_VERSION);
            // 150.sql created the table; 151.sql added its two columns.
            let cols: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_name = 'ttrss_scheduled_tasks' AND \
             (column_name = 'owner_uid' OR column_name = 'last_cron_expression')",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(cols, 2, "151.sql's columns are present");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A failing migration rolls back its file: the version stays at the last
/// applied one, and none of the failing file's statements survive.
#[tokio::test]
async fn failing_migration_rolls_back_its_file() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            make_v149(pool).await;

            let root = temp_sql_root(|pgsql| {
                // 150.sql (real) applies fine; 151.sql (doctored) fails partway.
                let p = pgsql.join(format!(
                    "migrations/{}.sql",
                    ttrss::core::config::SCHEMA_VERSION
                ));
                std::fs::write(
                    &p,
                    "alter table ttrss_scheduled_tasks add column owner_uid integer;\n\
                 insert into no_such_table values (1);\n",
                )
                .unwrap();
            });
            let mig = Migrations::from_root(root);

            let err = mig.migrate(pool).await.unwrap_err();
            assert!(
                err.to_string().contains("no_such_table"),
                "the failing statement surfaces: {err}"
            );
            assert_eq!(
                version_of(pool).await,
                150,
                "the version stays at the last fully-applied file"
            );
            // The doctored 151.sql's first statement (owner_uid) must be gone
            // — the whole file rolled back.
            let col: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_name = 'ttrss_scheduled_tasks' AND column_name = 'owner_uid'",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(col, 0, "the rolled-back file left no trace");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A migration file *above* the `SCHEMA_VERSION` ceiling is never applied.
#[tokio::test]
async fn migrations_above_the_ceiling_are_ignored() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            make_fresh(pool).await;

            let root = temp_sql_root(|pgsql| {
                std::fs::write(
                    pgsql.join("migrations/152.sql"),
                    "create table ttrss_probe_152 (id integer);",
                )
                .unwrap();
            });
            let mig = Migrations::from_root(root);
            assert_eq!(mig.get_max_version().await?, SCHEMA_VERSION, "the cap wins");
            mig.migrate(pool).await?;

            assert_eq!(version_of(pool).await, SCHEMA_VERSION);
            let probe: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_name = 'ttrss_probe_152'",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(probe, 0, "152.sql was never applied");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A DB stamped *above* the build's ceiling fails closed (d7).
#[tokio::test]
async fn db_newer_than_the_build_fails() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await?;
            let pool = db.pool();
            sqlx::query("UPDATE ttrss_version SET schema_version = 152")
                .execute(pool)
                .await
                .unwrap();

            let err = ref_migrations().migrate(pool).await.unwrap_err();
            assert!(
                err.to_string().contains("newer than this build"),
                "actionable ceiling error: {err}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
