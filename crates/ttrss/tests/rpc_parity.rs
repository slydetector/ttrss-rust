//! Test parity: 1:1 port of
//! `tt-rss/tests_integration/RPCTest.php` (10 cases).
//!
//! The PHP suite calls the `RPC` handler methods directly
//! (`(new RPC($request))->publ()` etc.) with `DbTestCase::setUp`'s
//! `$_SESSION["uid"] = 1`; the Rust port calls the same handler functions
//! (`ttrss::article::{publ, mark, delete, mark_selected,
//! publish_selected}`) with an explicit `Sess` carrying `uid = 1`. The
//! `$_REQUEST` payload is the `extra` parameter map.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;

/// A test-only config (the handlers only need `db` for these cases).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        db_max_connections: 10,
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: true,
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

/// `DbTestCase::setUp`: fresh fixtures + `$_SESSION["uid"] = 1`.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState {
        config: test_config(),
        db,
    };
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

fn message(result: &Value) -> &str {
    result.get("message").and_then(Value::as_str).unwrap_or("")
}

/// Read `(published, last_published)` for a ref id (owner 1).
async fn published_state(db: &Db, ref_id: i32) -> (bool, Option<chrono::NaiveDateTime>) {
    sqlx::query_as(
        "SELECT published, last_published FROM ttrss_user_entries
         WHERE ref_id = $1 AND owner_uid = 1",
    )
    .bind(ref_id)
    .fetch_one(db.pool())
    .await
    .expect("published state")
}

/// Read `(marked, last_marked)` for a ref id (owner 1).
async fn marked_state(db: &Db, ref_id: i32) -> (bool, Option<chrono::NaiveDateTime>) {
    sqlx::query_as(
        "SELECT marked, last_marked FROM ttrss_user_entries
         WHERE ref_id = $1 AND owner_uid = 1",
    )
    .bind(ref_id)
    .fetch_one(db.pool())
    .await
    .expect("marked state")
}

// PHP: test_publ_publish_article
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_publ_publish_article(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let ref_id = 1i32; // seed.sql entry with ref_id=1
    let (published, last_published) = published_state(db, ref_id).await;

    // Verify initial state: published should be false, last_published NULL
    assert!(!published);
    assert!(last_published.is_none());

    // Call publ to publish the article
    let result = ttrss::article::publ(
        &state,
        &sess,
        &request(&[("pub", json!("1")), ("id", json!("1"))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (published, last_published) = published_state(db, ref_id).await;
    assert!(published);
    assert!(last_published.is_some());
}

// PHP: test_publ_unpublish_article
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_publ_unpublish_article(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let ref_id = 3i32; // seed.sql entry with ref_id=3, initially marked=true

    // First publish it
    let _ = ttrss::article::publ(
        &state,
        &sess,
        &request(&[("pub", json!("1")), ("id", json!("3"))]),
    )
    .await;

    let (published, last_published) = published_state(db, ref_id).await;
    assert!(published);
    assert!(last_published.is_some());

    // Now unpublish it
    let result = ttrss::article::publ(
        &state,
        &sess,
        &request(&[("pub", json!("0")), ("id", json!("3"))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (published, last_published) = published_state(db, ref_id).await;
    assert!(!published);
    assert!(last_published.is_some());
}

// PHP: test_mark_star_article
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_mark_star_article(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let ref_id = 6i32; // seed.sql entry with ref_id=6, initially marked=false
    let (marked, last_marked) = marked_state(db, ref_id).await;

    // Verify initial state: marked should be false, last_marked NULL
    assert!(!marked);
    assert!(last_marked.is_none());

    // Call mark to star the article
    let result = ttrss::article::mark(
        &state,
        &sess,
        &request(&[("mark", json!("1")), ("id", json!("6"))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (marked, last_marked) = marked_state(db, ref_id).await;
    assert!(marked);
    assert!(last_marked.is_some());
}

// PHP: test_mark_unstar_article
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_mark_unstar_article(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let ref_id = 7i32; // seed.sql entry with ref_id=7, initially marked=false

    // First star it
    let _ = ttrss::article::mark(
        &state,
        &sess,
        &request(&[("mark", json!("1")), ("id", json!("7"))]),
    )
    .await;

    let (marked, last_marked) = marked_state(db, ref_id).await;
    assert!(marked);
    assert!(last_marked.is_some());

    // Now un-star it
    let result = ttrss::article::mark(
        &state,
        &sess,
        &request(&[("mark", json!("0")), ("id", json!("7"))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (marked, last_marked) = marked_state(db, ref_id).await;
    assert!(!marked);
    assert!(last_marked.is_some());
}

// PHP: test_publ_updates_last_published_timestamp
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_publ_updates_last_published_timestamp(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let ref_id = 5i32;

    // First publish
    let _ = ttrss::article::publ(
        &state,
        &sess,
        &request(&[("pub", json!("1")), ("id", json!("5"))]),
    )
    .await;

    let (_, first_published) = published_state(db, ref_id).await;

    // Wait a moment and publish again (PHP: usleep(1000010), ~1 second).
    // Hold a connection across the sleep so this test's database is never
    // connection-less: the harness pool has a 1s idle timeout, and sibling
    // test binaries' one-shot stale-DB cleanup drops connection-less test
    // databases older than 2 seconds.
    let mut _hold = db.pool().acquire().await.expect("hold connection");
    tokio::time::sleep(std::time::Duration::from_millis(1010)).await;

    let _ = ttrss::article::publ(
        &state,
        &sess,
        &request(&[("pub", json!("1")), ("id", json!("5"))]),
    )
    .await;

    let (_, second_published) = published_state(db, ref_id).await;

    // The second timestamp should be >= the first
    assert!(
        second_published >= first_published,
        "second {:?} >= first {:?}",
        second_published,
        first_published
    );
}

// PHP: test_delete_removes_articles
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_delete_removes_articles(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Verify articles exist before deletion
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE ref_id IN (4, 5) AND owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(2, count);

    // Delete articles with ref_id 4 and 5
    let result = ttrss::article::delete(&state, &sess, &request(&[("ids", json!("4,5"))])).await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    // Verify articles are gone
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE ref_id IN (4, 5) AND owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(0, count);
}

// PHP: test_markSelected_mark_as_read
#[allow(non_snake_case)]
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_markSelected_mark_as_read(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Verify initial state: ref_id 9 should be unmarked
    let (marked, _) = marked_state(db, 9).await;
    assert!(!marked);

    // Mark as read via markSelected (cmode=0 = mark as read)
    let result = ttrss::article::mark_selected(
        &state,
        &sess,
        &request(&[("ids", json!([9])), ("cmode", json!(0))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    // Verify marked=false
    let (marked, _) = marked_state(db, 9).await;
    assert!(!marked);
}

// PHP: test_markSelected_mark_as_unread
#[allow(non_snake_case)]
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_markSelected_mark_as_unread(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // First mark as read
    let _ = ttrss::article::mark_selected(
        &state,
        &sess,
        &request(&[("ids", json!([10])), ("cmode", json!(0))]),
    )
    .await;

    let (marked, _) = marked_state(db, 10).await;
    assert!(!marked);

    // Now mark as unread (cmode=1)
    let result = ttrss::article::mark_selected(
        &state,
        &sess,
        &request(&[("ids", json!([10])), ("cmode", json!(1))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (marked, _) = marked_state(db, 10).await;
    assert!(marked);
}

// PHP: test_publishSelected_publish_articles
#[allow(non_snake_case)]
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_publishSelected_publish_articles(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Verify initial state: ref_id 11 should be read and unpublished
    let (published, _) = published_state(db, 11).await;
    assert!(!published);

    // Publish via publishSelected (cmode=1 = mark as unread => published=true)
    let result = ttrss::article::publish_selected(
        &state,
        &sess,
        &request(&[("ids", json!([11])), ("cmode", json!(1))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (published, _) = published_state(db, 11).await;
    assert!(published);
}

// PHP: test_publishSelected_unpublish_articles
#[allow(non_snake_case)]
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_publishSelected_unpublish_articles(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // First publish the article
    let _ = ttrss::article::publish_selected(
        &state,
        &sess,
        &request(&[("ids", json!([11])), ("cmode", json!(1))]),
    )
    .await;

    let (published, _) = published_state(db, 11).await;
    assert!(published);

    // Now unpublish (cmode=0 = mark as read => published=false)
    let result = ttrss::article::publish_selected(
        &state,
        &sess,
        &request(&[("ids", json!([11])), ("cmode", json!(0))]),
    )
    .await;

    assert_eq!("UPDATE_COUNTERS", message(&result));

    let (published, _) = published_state(db, 11).await;
    assert!(!published);
}
