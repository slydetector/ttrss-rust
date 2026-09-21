//! M6c.3 — the daemon/scheduler loop (`ttrss::updates::{select_due_feeds, claim_feeds,
//! daemon_loop}`).
//!
//! These are DB-backed tests that cover the due-feeds selection (the
//! `update_daemon_common` SQL), the claim (`last_update_started = NOW()`), and the
//! graceful-shutdown path. The per-feed dispatch (the `JoinSet`/`Semaphore`) is
//! covered by the M6c.2 workhorse tests; here we focus on the selection + claim +
//! shutdown.

mod common;

use sqlx::pool::PoolOptions;
use sqlx::postgres::PgConnectOptions;
use std::sync::Arc;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::updates::{claim_feeds, select_due_feeds, DueFeed};
use ttrss::pluginhost::PluginHost;

/// A test `Config` with the daemon defaults + a short sleep interval.
fn test_config() -> Config {
    Config {
        db_host: String::new(),
        db_port: 0,
        db_user: String::new(),
        db_pass: String::new(),
        db_name: String::new(),
        db_sslmode: String::new(),
        db_max_connections: 10,
        self_url: String::new(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: String::new(),
        listen: "127.0.0.1:0".parse().unwrap(),
        docroot: std::path::PathBuf::new(),
        feed_icon_dir: std::path::PathBuf::new(),
        max_favicon_file_size: 0,
        max_cache_file_size: 64 * 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 5,
        feed_fetch_no_cache_timeout: 3,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: false,
        daemon_sleep_interval: 60,
        daemon_max_jobs: 2,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

/// Insert a user (with a recent `last_login`) and return its id.
async fn create_user(db: &Db, uid: i32, access_level: i32) -> i32 {
    sqlx::query(
        "INSERT INTO ttrss_users (id, login, pwd_hash, access_level, last_login) \
         VALUES ($1, $2, 'SHA1:00', $3, NOW())",
    )
    .bind(uid)
    .bind(format!("test-user-{uid}"))
    .bind(access_level)
    .execute(db.pool())
    .await
    .expect("create user");
    uid
}

/// Insert a feed (with the given `update_interval` + `last_updated`) and return its id.
async fn create_feed(
    db: &Db,
    id: i32,
    owner_uid: i32,
    update_interval: i32,
    last_updated: &str, // "NULL" | "NOW()" | "NOW() - INTERVAL '2 hour'" | etc.
) -> i32 {
    let sql = format!(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url, update_interval, last_updated) \
         VALUES ($1, $2, $3, $4, $5, {last_updated})"
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(owner_uid)
        .bind(format!("Test Feed {id}"))
        .bind(format!("http://example.com/feed-{id}"))
        .bind(update_interval)
        .execute(db.pool())
        .await
        .expect("create feed");
    id
}

/// The feed's `last_update_started` (non-null when claimed).
async fn last_update_started_is_set(db: &Db, feed_id: i32) -> bool {
    sqlx::query_scalar(
        "SELECT last_update_started IS NOT NULL FROM ttrss_feeds WHERE id = $1",
    )
    .bind(feed_id)
    .fetch_one(db.pool())
    .await
    .expect("last_update_started")
}


// A feed with `update_interval = 0` (use the default interval) and `last_updated`
// older than the default interval (30 min) is due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_default_interval(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 300, 0).await;
    let feed_id = create_feed(&db, 300, uid, 0, "NOW() - INTERVAL '2 hour'").await;

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        ids.contains(&feed_id),
        "expected feed {feed_id} to be due; got {ids:?}"
    );
}

// A feed with `update_interval = 0` and `last_updated` recent (within the default
// interval) is NOT due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_not_due(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 301, 0).await;
    let feed_id = create_feed(&db, 301, uid, 0, "NOW()").await;

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        !ids.contains(&feed_id),
        "expected feed {feed_id} to NOT be due; got {ids:?}"
    );
}

// A feed with `update_interval = -1` (disabled) is NOT due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_disabled(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 302, 0).await;
    let feed_id = create_feed(&db, 302, uid, -1, "NOW() - INTERVAL '2 hour'").await;

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        !ids.contains(&feed_id),
        "expected feed {feed_id} (disabled) to NOT be due; got {ids:?}"
    );
}

// A feed with `last_updated` NULL (never updated) is always due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_never_updated(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 303, 0).await;
    let feed_id = create_feed(&db, 303, uid, 0, "NULL").await;

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        ids.contains(&feed_id),
        "expected feed {feed_id} (never updated) to be due; got {ids:?}"
    );
}

// A feed owned by a disabled user (access_level = -2) is NOT due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_disabled_owner(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 304, -2 /* DISABLED */).await;
    let feed_id = create_feed(&db, 304, uid, 0, "NOW() - INTERVAL '2 hour'").await;

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        !ids.contains(&feed_id),
        "expected feed {feed_id} (disabled owner) to NOT be due; got {ids:?}"
    );
}

// A feed currently being updated (`last_update_started` recent) is NOT due.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_select_due_feeds_in_progress(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 305, 0).await;
    let feed_id = create_feed(&db, 305, uid, 0, "NOW() - INTERVAL '2 hour'").await;
    // Mark the feed as currently being updated (the in-progress marker).
    sqlx::query("UPDATE ttrss_feeds SET last_update_started = NOW() WHERE id = $1")
        .bind(feed_id)
        .execute(db.pool())
        .await
        .expect("mark in-progress");

    let due = select_due_feeds(db.pool(), &cfg, 50).await;
    let ids: Vec<i32> = due.iter().map(|f| f.id).collect();
    assert!(
        !ids.contains(&feed_id),
        "expected feed {feed_id} (in progress) to NOT be due; got {ids:?}"
    );
}

// The claim sets `last_update_started = NOW()` for the due feeds.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_claim_feeds(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 306, 0).await;
    let feed_id = create_feed(&db, 306, uid, 0, "NOW() - INTERVAL '2 hour'").await;
    assert!(!last_update_started_is_set(&db, feed_id).await);

    let due = vec![DueFeed {
        id: feed_id,
        owner_uid: uid,
        feed_title: String::new(),
    }];
    claim_feeds(db.pool(), &due).await;

    assert!(last_update_started_is_set(&db, feed_id).await);
}

// The daemon loop exits when the `CancellationToken` is cancelled.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_daemon_loop_graceful_shutdown(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let cfg = test_config();
    let cancel = tokio_util::sync::CancellationToken::new();

    let handle = tokio::spawn({
        let pool = db.pool().clone();
        let cfg = cfg.clone();
        let cancel = cancel.clone();
        async move {
            ttrss::updates::daemon_loop(&pool, &cfg, Arc::new(PluginHost::startup(&[])), cancel).await;
        }
    });

    // Give the loop a moment to start (the first tick is immediate; the
    // `select_due_feeds` query runs; the loop sleeps the interval).
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Cancel the token (the loop exits on the `cancel.cancelled()` branch).
    cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("daemon loop exited on cancel");
}

