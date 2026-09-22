//! M6c.4 — housekeeping ("task 0"): the cron-driven scheduled tasks
//! (`ttrss::scheduler`) + the per-feed purge (`ttrss::feeds::purge_feed`).
//!
//! DB-backed tests that mirror the PHP's `Scheduler` /
//! `RSSUtils::init_housekeeping_tasks` / `Article::_purge_orphans` /
//! `RSSUtils::disable_failed_feeds` / `RSSUtils::expire_error_log` /
//! `RSSUtils::cleanup_feed_icons` and `Feeds::_purge`.

mod common;

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _};
use sqlx::AssertSqlSafe;
use croner::Cron;
use std::str::FromStr;
use sqlx::pool::PoolOptions;
use sqlx::postgres::PgConnectOptions;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use test_db::template::with_test_db;

/// A test `Config` with the housekeeping defaults.
fn test_config() -> Config { common::test_config() }

/// Insert a user and return its id.
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

/// Insert a feed owned by `owner_uid` and return its id.
async fn create_feed(db: &Db, id: i32, owner_uid: i32, feed_url: &str) -> i32 {
    sqlx::query("INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(owner_uid)
        .bind(format!("Test Feed {id}"))
        .bind(feed_url)
        .execute(db.pool())
        .await
        .expect("create feed");
    id
}

/// Insert a `ttrss_entries` row (a minimal valid entry) and return its id.
async fn create_entry(db: &Db, guid: &str) -> i32 {
    sqlx::query_scalar(
        "INSERT INTO ttrss_entries (title, guid, link, updated, content, content_hash, \
         date_entered, date_updated) VALUES ($1, $2, $3, NOW(), '', 'hash', NOW(), NOW()) \
         RETURNING id",
    )
    .bind(format!("Entry {guid}"))
    .bind(guid)
    .bind(format!("http://example.com/{guid}"))
    .fetch_one(db.pool())
    .await
    .expect("create entry")
}

/// Insert a `ttrss_user_entries` row linking `ref_id` to `feed_id`/`owner_uid`.
async fn create_user_entry(
    db: &Db,
    ref_id: i32,
    feed_id: i32,
    owner_uid: i32,
    marked: bool,
    unread: bool,
) {
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, marked, unread, \
         tag_cache, label_cache) VALUES ($1, $2, $3, $4, $5, $6, '', '')",
    )
    .bind(ref_id)
    .bind(format!("uuid-{ref_id}"))
    .bind(feed_id)
    .bind(owner_uid)
    .bind(marked)
    .bind(unread)
    .execute(db.pool())
    .await
    .expect("create user entry");
}

/// Count the `ttrss_user_entries` rows for a feed.
async fn user_entry_count(db: &Db, feed_id: i32) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries WHERE feed_id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("user entry count")
}

/// Age a `ttrss_entries` row's `date_updated` (and `updated`) by N days.
async fn age_entry(db: &Db, entry_id: i32, days: i64) {
    sqlx::query(
        AssertSqlSafe(format!(
            "UPDATE ttrss_entries SET date_updated = NOW() - INTERVAL '{days} day', \
             updated = NOW() - INTERVAL '{days} day' WHERE id = $1",
        )),
    )
    .bind(entry_id)
    .execute(db.pool())
    .await
    .expect("age entry");
}

/// Set a file's mtime to an absolute `SystemTime` (ages it for the icon test).
fn set_mtime(path: &std::path::Path, time: std::time::SystemTime) {
    std::fs::File::open(path).and_then(|f| f.set_modified(time)).expect("set mtime");
}

// ---------------------------------------------------------------------------
// is_due (pure cron logic)
// ---------------------------------------------------------------------------

#[test]
fn test_is_due_daily() {
    let cron = Cron::from_str("@daily").unwrap();
    let last_run = NaiveDateTime::new(
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    );
    // `now` is 2 days later → the next daily run (2024-01-02 00:00) is in the past.
    let now = Local
        .from_local_datetime(
            &NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2024, 1, 3).unwrap(),
                NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
        )
        .unwrap();
    assert!(ttrss::scheduler::is_due(&cron, last_run, now));

    // `now` is before the next daily run → not due.
    let now_early = Local
        .from_local_datetime(
            &NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
                NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            ),
        )
        .unwrap();
    assert!(!ttrss::scheduler::is_due(&cron, last_run, now_early));
}

#[test]
fn test_is_due_hourly() {
    let cron = Cron::from_str("@hourly").unwrap();
    let last_run = NaiveDateTime::new(
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    );
    let now = Local
        .from_local_datetime(
            &NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
                NaiveTime::from_hms_opt(11, 30, 0).unwrap(),
            ),
        )
        .unwrap();
    // The next hourly run (11:00) is before now (11:30) → due.
    assert!(ttrss::scheduler::is_due(&cron, last_run, now));
}

// ---------------------------------------------------------------------------
// purge_orphans
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_purge_orphans(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let uid = create_user(&db, 400, 0).await;
    let feed_id = create_feed(&db, 400, uid, "http://example.com/f400").await;

    // An entry that has a user_entry (should survive).
    let kept = create_entry(&db, "kept-400").await;
    create_user_entry(&db, kept, feed_id, uid, false, true).await;

    // An entry that has NO user_entry (an orphan — should be deleted).
    let _orphan = create_entry(&db, "orphan-400").await;

    let deleted = ttrss::scheduler::purge_orphans(db.pool()).await.expect("purge_orphans");
    assert_eq!(deleted, 1, "exactly one orphan should be deleted");

    // The kept entry's user_entry still exists.
    assert_eq!(user_entry_count(&db, feed_id).await, 1);
    // The orphan entry is gone.
    let orphan_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries WHERE guid = 'orphan-400'")
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(orphan_left, 0);
}

// ---------------------------------------------------------------------------
// disable_failed_feeds
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_disable_failed_feeds(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config(); // daemon_unsuccessful_days_limit = 30
    let uid = create_user(&db, 401, 0).await;

    // A feed that failed to update for > 30 days but is still being attempted
    // (last_updated within the last day) → should be disabled.
    let failing = create_feed(&db, 401, uid, "http://example.com/f401").await;
    sqlx::query(
        "UPDATE ttrss_feeds SET last_successful_update = NOW() - INTERVAL '60 day', \
         last_updated = NOW() - INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(failing)
    .execute(db.pool())
    .await
    .expect("stamp failing feed");

    // A feed that failed > 30 days ago but is NOT being attempted (last_updated
    // old) → should NOT be disabled.
    let stale = create_feed(&db, 402, uid, "http://example.com/f402").await;
    sqlx::query(
        "UPDATE ttrss_feeds SET last_successful_update = NOW() - INTERVAL '60 day', \
         last_updated = NOW() - INTERVAL '2 day' WHERE id = $1",
    )
    .bind(stale)
    .execute(db.pool())
    .await
    .expect("stamp stale feed");

    let disabled = ttrss::scheduler::disable_failed_feeds(db.pool(), &cfg).await.expect("disable");
    assert_eq!(disabled, 1, "exactly one feed should be disabled");

    let failing_interval: i32 = sqlx::query_scalar("SELECT update_interval FROM ttrss_feeds WHERE id = $1")
        .bind(failing)
        .fetch_one(db.pool())
        .await
        .expect("failing interval");
    assert_eq!(failing_interval, -1, "failing feed should be disabled");

    let stale_interval: i32 = sqlx::query_scalar("SELECT update_interval FROM ttrss_feeds WHERE id = $1")
        .bind(stale)
        .fetch_one(db.pool())
        .await
        .expect("stale interval");
    assert_eq!(stale_interval, 0, "stale feed should be left alone");
}

// ---------------------------------------------------------------------------
// expire_error_log
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_expire_error_log(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");

    // An old error (should be deleted) and a recent one (should survive).
    sqlx::query(
        "INSERT INTO ttrss_error_log (owner_uid, errno, errstr, filename, lineno, context, created_at) \
         VALUES (NULL, 0, 'old', 'f.php', 1, '', NOW() - INTERVAL '2 week')",
    )
    .execute(db.pool())
    .await
    .expect("insert old error");
    sqlx::query(
        "INSERT INTO ttrss_error_log (owner_uid, errno, errstr, filename, lineno, context, created_at) \
         VALUES (NULL, 0, 'recent', 'f.php', 1, '', NOW())",
    )
    .execute(db.pool())
    .await
    .expect("insert recent error");

    let deleted = ttrss::scheduler::expire_error_log(db.pool()).await.expect("expire");
    assert_eq!(deleted, 1, "exactly one old error should be deleted");

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_error_log")
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(remaining, 1);
    let errstr: String = sqlx::query_scalar("SELECT errstr FROM ttrss_error_log")
        .fetch_one(db.pool())
        .await
        .expect("errstr");
    assert_eq!(errstr, "recent");
}

// ---------------------------------------------------------------------------
// cleanup_feed_icons
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_cleanup_feed_icons(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let mut cfg = test_config(); // cache_max_days = 7
    let uid = create_user(&db, 403, 0).await;

    // A temp dir to act as the feed-icon cache.
    let dir = std::env::temp_dir().join(format!("ttrss_icons_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create icon dir");
    cfg.feed_icon_dir = dir.clone();

    // A feed that EXISTS (icon should be kept/refreshed) and a feed id that does
    // NOT exist (icon should be deleted).
    let existing_feed = create_feed(&db, 403, uid, "http://example.com/f403").await;
    let missing_feed_id = 99999;

    // Write two icon files and age them to 10 days (past cache_max_days).
    let existing_icon = dir.join(existing_feed.to_string());
    std::fs::write(&existing_icon, b"icon-existing").expect("write existing icon");
    let missing_icon = dir.join(missing_feed_id.to_string());
    std::fs::write(&missing_icon, b"icon-missing").expect("write missing icon");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86400 * 10);
    set_mtime(&existing_icon, old);
    set_mtime(&missing_icon, old);

    let removed = ttrss::scheduler::cleanup_feed_icons(db.pool(), &cfg).await.expect("cleanup");
    assert_eq!(removed, 1, "exactly one orphaned icon should be removed");
    assert!(existing_icon.exists(), "existing feed's icon should be kept");
    assert!(!missing_icon.exists(), "missing feed's icon should be removed");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// record_run + run_due_tasks
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_record_run_upsert(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");

    // Insert.
    ttrss::scheduler::record_run(db.pool(), "test_task", 3, 0, "@daily")
        .await
        .expect("insert record");
    let row: (i32, i32, String) = sqlx::query_as(
        "SELECT last_duration, last_rc, last_cron_expression \
         FROM ttrss_scheduled_tasks WHERE task_name = 'test_task'",
    )
    .fetch_one(db.pool())
    .await
    .expect("fetch record");
    assert_eq!(row, (3, 0, "@daily".to_string()));

    // Upsert (update the same row).
    ttrss::scheduler::record_run(db.pool(), "test_task", 5, -100, "@hourly")
        .await
        .expect("update record");
    let row2: (i32, i32, String) = sqlx::query_as(
        "SELECT last_duration, last_rc, last_cron_expression \
         FROM ttrss_scheduled_tasks WHERE task_name = 'test_task'",
    )
    .fetch_one(db.pool())
    .await
    .expect("fetch record 2");
    assert_eq!(row2, (5, -100, "@hourly".to_string()));

    // Still exactly one row.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_scheduled_tasks WHERE task_name = 'test_task'")
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(count, 1);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_run_due_tasks_runs_due_only(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();

    // Register two custom tasks: one due (cron `@hourly`, never run → normalizes
    // to `0 * * * *`), one not due (cron `@daily`, last_run just now).
    let tasks = vec![
        ttrss::scheduler::ScheduledTask {
            name: "expire_error_log",
            cron_expression: "@hourly".to_string(),
        },
        ttrss::scheduler::ScheduledTask {
            name: "purge_orphans",
            cron_expression: "@daily".to_string(),
        },
    ];

    // Seed the "not due" task with a recent last_run so `@daily` is not due.
    sqlx::query(
        "INSERT INTO ttrss_scheduled_tasks (task_name, last_duration, last_rc, last_run, last_cron_expression) \
         VALUES ('purge_orphans', 0, 0, NOW(), '@daily')",
    )
    .execute(db.pool())
    .await
    .expect("seed not-due task");

    // Seed an old error log row so `expire_error_log` has something to do.
    sqlx::query(
        "INSERT INTO ttrss_error_log (owner_uid, errno, errstr, filename, lineno, context, created_at) \
         VALUES (NULL, 0, 'old', 'f.php', 1, '', NOW() - INTERVAL '2 week')",
    )
    .execute(db.pool())
    .await
    .expect("seed old error");

    ttrss::scheduler::run_due_tasks(db.pool(), &cfg, &tasks).await;

    // The due task (`expire_error_log`) ran and recorded itself.
    let expire_recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ttrss_scheduled_tasks WHERE task_name = 'expire_error_log')",
    )
    .fetch_one(db.pool())
    .await
    .expect("expire recorded");
    assert!(expire_recorded, "the due task should have run and been recorded");
    // The stored expression is the normalized form (the PHP's `getExpression()`):
    // `@hourly` → `0 * * * *`.
    let stored_expr: String = sqlx::query_scalar(
        "SELECT last_cron_expression FROM ttrss_scheduled_tasks WHERE task_name = 'expire_error_log'",
    )
    .fetch_one(db.pool())
    .await
    .expect("stored expr");
    assert_eq!(stored_expr, "0 * * * *", "the normalized expression should be stored");

    // The old error was deleted (the due task's effect).
    let errors_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_error_log")
        .fetch_one(db.pool())
        .await
        .expect("errors left");
    assert_eq!(errors_left, 0, "the due task should have expired the old error");

    // The not-due task (`purge_orphans`) was NOT re-run: it has exactly the one
    // record we seeded (no second row created).
    let purge_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_scheduled_tasks WHERE task_name = 'purge_orphans'",
    )
    .fetch_one(db.pool())
    .await
    .expect("purge count");
    assert_eq!(purge_count, 1, "the not-due task should not have created a new record");
}

// ---------------------------------------------------------------------------
// purge_feed (Feeds::_purge)
// ---------------------------------------------------------------------------

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_purge_feed(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 404, 0).await;
    let feed_id = create_feed(&db, 404, uid, "http://example.com/f404").await;

    // Set the feed's purge_interval to 7 days (so purge_feed(feed_id, 0) uses 7).
    sqlx::query("UPDATE ttrss_feeds SET purge_interval = 7 WHERE id = $1")
        .bind(feed_id)
        .execute(db.pool())
        .await
        .expect("set purge interval");

    // An old, unmarked, unread entry (should be purged).
    let old_unread = create_entry(&db, "old-unread-404").await;
    create_user_entry(&db, old_unread, feed_id, uid, false, true).await;
    // An old, unmarked, read entry (should be purged — read is always purged).
    let old_read = create_entry(&db, "old-read-404").await;
    create_user_entry(&db, old_read, feed_id, uid, false, false).await;
    // An old, MARKED entry (should survive — marked is never purged).
    let old_marked = create_entry(&db, "old-marked-404").await;
    create_user_entry(&db, old_marked, feed_id, uid, true, true).await;
    // A recent, unmarked, unread entry (should survive — not old enough).
    let recent_unread = create_entry(&db, "recent-unread-404").await;
    create_user_entry(&db, recent_unread, feed_id, uid, false, true).await;

    // Age the three "old" entries to 10 days (past the 7-day purge interval).
    age_entry(&db, old_unread, 10).await;
    age_entry(&db, old_read, 10).await;
    age_entry(&db, old_marked, 10).await;

    let deleted = ttrss::feeds::purge_feed(db.pool(), &cfg, feed_id, 0).await;
    assert_eq!(deleted, Some(2), "two old unmarked entries should be purged");
    assert_eq!(user_entry_count(&db, feed_id).await, 2, "marked + recent should survive");

    // Marked entry still present.
    let marked_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries WHERE ref_id = $1")
        .bind(old_marked)
        .fetch_one(db.pool())
        .await
        .expect("marked left");
    assert_eq!(marked_left, 1);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_purge_feed_disabled(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let uid = create_user(&db, 405, 0).await;
    // A feed with purge_interval = 0; set the owner's PURGE_OLD_DAYS pref to 0
    // so the effective interval is 0 (disabled).
    let feed_id = create_feed(&db, 405, uid, "http://example.com/f405").await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, value, profile) \
         VALUES ($1, 'PURGE_OLD_DAYS', '0', NULL)",
    )
    .bind(uid)
    .execute(db.pool())
    .await
    .expect("set purge pref 0");

    let old = create_entry(&db, "old-405").await;
    create_user_entry(&db, old, feed_id, uid, false, true).await;
    age_entry(&db, old, 70).await;

    let deleted = ttrss::feeds::purge_feed(db.pool(), &cfg, feed_id, 0).await;
    assert_eq!(deleted, None, "purging should be disabled (interval 0)");
    assert_eq!(user_entry_count(&db, feed_id).await, 1, "the entry should survive");
}
