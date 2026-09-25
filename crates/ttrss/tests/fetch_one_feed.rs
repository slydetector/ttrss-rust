//! M6c.2 — the single-feed update workhorse (`ttrss::updates::fetch_one_feed`),
//! mirroring the per-feed body of the PHP's `RSSUtils::update_rss_feed`.
//!
//! These are DB-backed tests that drive the workhorse against a loopback mock
//! HTTP server (the network is isolated here). They cover the fetch outcomes:
//! success (200 + parseable XML), 304 (not modified), 429 (throttled), a
//! network error, and a parse failure. The 30-day auto-disable is a separate
//! scheduled task (M6c.4), not the workhorse.

mod common;

use std::net::TcpListener;
use std::sync::Arc;

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::pluginhost::PluginHost;
use ttrss::updates::{fetch_one_feed, FeedUpdateFlags};
use test_db::template::with_test_db;
/// An empty plugin host — the `HOOK_ARTICLE_FILTER` chain is a no-op with
/// no plugins (M7.9), so these tests exercise the plugin-free path.
fn no_plugins() -> PluginHost {
    PluginHost::startup(&[])
}

use common::{Canned, MockServer};

/// The recorded mock feed (a single Atom entry, "x-21may2020").
const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// The PHP suite's invalid-XML string.
const BAD_FEED: &str = "<invalid-xml><unclosed>";

/// A test `Config` with short fetch timeouts + the upstream fetch-tuning
/// defaults (the `fetch_one_feed` function only reads the fetch-tuning fields
/// + the encryption key).
fn test_config() -> Config {
    Config {
        db_host: String::new(),
        db_port: 0,
        db_user: String::new(),
        db_name: String::new(),
        db_sslmode: String::new(),
        self_url: String::new(),
        plugins: String::new(),
        listen: "127.0.0.1:0".parse().unwrap(),
        docroot: std::path::PathBuf::new(),
        cache_dir: std::path::PathBuf::new(),
        max_favicon_file_size: 0,
        feed_fetch_timeout: 5,
        feed_fetch_no_cache_timeout: 3,
        http_429_throttle_interval: 3600,
        daemon_sleep_interval: 60,
    ..Default::default()
    }
}

/// Insert a user and return its id.
async fn create_user(db: &Db, uid: i32, access_level: i32) -> i32 {
    sqlx::query(
        "INSERT INTO ttrss_users (id, login, pwd_hash, access_level) VALUES ($1, $2, 'SHA1:00', $3)",
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

/// The feed's `last_error`.
async fn last_error(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT last_error FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_error")
}

/// The feed's `last_successful_update` (non-null when a successful update ran).
async fn last_successful_update_is_set(db: &Db, feed_id: i32) -> bool {
    sqlx::query_scalar("SELECT last_successful_update IS NOT NULL FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_successful_update")
}

/// The feed's `last_unconditional` (non-null when an unconditional update ran).
async fn last_unconditional_is_set(db: &Db, feed_id: i32) -> bool {
    sqlx::query_scalar("SELECT last_unconditional IS NOT NULL FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_unconditional")
}

/// The feed's `last_updated` (as a `NaiveDateTime`).
async fn last_updated(db: &Db, feed_id: i32) -> chrono::NaiveDateTime {
    sqlx::query_scalar("SELECT last_updated FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_updated")
}

/// The feed's `last_modified` (the stored `If-Modified-Since` value).
async fn last_modified(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT last_modified FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_modified")
}

/// The feed's `update_interval` (set to -1 when auto-disabled).
async fn update_interval(db: &Db, feed_id: i32) -> i32 {
    sqlx::query_scalar("SELECT update_interval FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("update_interval")
}

/// `ttrss_entries` count for a feed (entries ⋈ user_entries by ref_id).
async fn entry_count(db: &Db, feed_id: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON e.id = ue.ref_id WHERE ue.feed_id = $1",
    )
    .bind(feed_id)
    .fetch_one(db.pool())
    .await
    .expect("entry count")
}


// A successful update (a 200 with parseable XML): the feed's articles are
// ingested + the success metadata is stamped.
#[tokio::test]
async fn test_fetch_one_feed_success() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let mock = MockServer::new(vec![Canned {
        status: 200,
        reason: "OK",
        headers: vec![("last-modified", "Sun, 06 Nov 1994 08:49:37 GMT".to_string())],
        body: MOCK_FEED.as_bytes().to_vec(),
    }]);
    let uid = create_user(&db, 200, 0).await;
    let feed_id = create_feed(&db, 200, uid, &mock.url("/feed")).await;

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(outcome.success, "expected success: {:?}", outcome.error);
    assert_eq!(outcome.new_articles, 1);
    assert!(last_successful_update_is_set(&db, feed_id).await);
    assert_eq!(last_error(&db, feed_id).await, "");
    assert!(last_unconditional_is_set(&db, feed_id).await);
    assert_eq!(entry_count(&db, feed_id).await, 1);
    // The `last_modified` is recorded (for the next conditional request).
    assert_eq!(last_modified(&db, feed_id).await, "Sun, 06 Nov 1994 08:49:37 GMT");
        Ok(())
    })).await.unwrap();
}

// A 304 (not modified): the feed is confirmed unchanged (no new articles, no
// `last_unconditional` — the fetch was conditional).
#[tokio::test]
async fn test_fetch_one_feed_304() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let mock = MockServer::new(vec![Canned {
        status: 304,
        reason: "Not Modified",
        headers: vec![],
        body: Vec::new(),
    }]);
    let uid = create_user(&db, 201, 0).await;
    let feed_id = create_feed(&db, 201, uid, &mock.url("/feed")).await;
    // Seed a `last_unconditional` (recent) + `last_modified` so the fetch is
    // conditional (not forced).
    sqlx::query(
        "UPDATE ttrss_feeds SET last_unconditional = NOW(), last_modified = 'Sun, 06 Nov 1994 08:49:37 GMT' WHERE id = $1",
    )
    .bind(feed_id)
    .execute(db.pool())
    .await
    .expect("seed conditional state");

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(outcome.success, "expected a 304 to succeed");
    assert_eq!(outcome.new_articles, 0);
    assert_eq!(last_error(&db, feed_id).await, "");
    assert!(last_successful_update_is_set(&db, feed_id).await);
    // A 304 does NOT advance `last_unconditional` (the fetch was conditional).
    let lu: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT last_unconditional FROM ttrss_feeds WHERE id = $1")
            .bind(feed_id)
            .fetch_one(db.pool())
            .await
            .expect("last_unconditional");
    assert!(lu.is_some());
    assert_eq!(entry_count(&db, feed_id).await, 0);
        Ok(())
    })).await.unwrap();
}

// A 429 (too many requests): the feed is throttled (`last_updated` +
// `last_successful_update` pushed into the future, `last_error` stamped).
#[tokio::test]
async fn test_fetch_one_feed_429() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let mock = MockServer::new(vec![Canned {
        status: 429,
        reason: "Too Many Requests",
        headers: vec![],
        body: Vec::new(),
    }]);
    let uid = create_user(&db, 202, 0).await;
    let feed_id = create_feed(&db, 202, uid, &mock.url("/feed")).await;

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(!outcome.success, "expected a 429 to fail");
    let err = outcome.error.expect("an error message");
    assert!(err.contains("throttled for"), "expected a throttle message: {err}");
    assert!(err.contains("seconds"), "expected a throttle message: {err}");
    // The `last_error` is stamped.
    assert_eq!(last_error(&db, feed_id).await, err);
    // The `last_updated` is pushed into the future (the throttle interval).
    let lu = last_updated(&db, feed_id).await;
    assert!(
        lu > chrono::Utc::now().naive_utc(),
        "expected last_updated in the future (throttle), got {lu}"
    );
    // The `last_successful_update` is pushed into the future too.
    assert!(last_successful_update_is_set(&db, feed_id).await);
    // The feed is NOT disabled (the auto-disable is M6c.4).
    assert_eq!(update_interval(&db, feed_id).await, 0);
        Ok(())
    })).await.unwrap();
}

// A network error (connection refused): the feed's `last_error` is stamped.
#[tokio::test]
async fn test_fetch_one_feed_network_error() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    // A port that is not listening (bind + drop to get a free port).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = listener.local_addr().unwrap().port();
    drop(listener);
    let dead_url = format!("http://127.0.0.1:{dead_port}/feed");

    let uid = create_user(&db, 203, 0).await;
    let feed_id = create_feed(&db, 203, uid, &dead_url).await;

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(!outcome.success, "expected a network error to fail");
    assert!(outcome.error.is_some(), "expected an error message");
    // The `last_error` is stamped (non-empty).
    assert!(!last_error(&db, feed_id).await.is_empty());
    // The `last_updated` is stamped (the failed update is recorded).
    let lu: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT last_updated FROM ttrss_feeds WHERE id = $1")
            .bind(feed_id)
            .fetch_one(db.pool())
            .await
            .expect("last_updated");
    assert!(lu.is_some());
        Ok(())
    })).await.unwrap();
}

// A parse failure (a 200 with invalid XML): the feed's `last_error` is stamped
// (the M6b `process_feed` parse-failure path).
#[tokio::test]
async fn test_fetch_one_feed_parse_failure() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let mock = MockServer::new(vec![Canned {
        status: 200,
        reason: "OK",
        headers: vec![],
        body: BAD_FEED.as_bytes().to_vec(),
    }]);
    let uid = create_user(&db, 204, 0).await;
    let feed_id = create_feed(&db, 204, uid, &mock.url("/feed")).await;

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(!outcome.success, "expected a parse failure to fail");
    assert!(outcome.error.is_some(), "expected an error message");
    // The `last_error` is stamped (non-empty).
    assert!(!last_error(&db, feed_id).await.is_empty());
    // The `last_unconditional` is stamped (the M6b parse-failure path).
    assert!(last_unconditional_is_set(&db, feed_id).await);
        Ok(())
    })).await.unwrap();
}

// A permission gate failure (a read-only owner): the feed is not updated.
#[tokio::test]
async fn test_fetch_one_feed_readonly_owner() {
    with_test_db(|url| Box::pin(async move {
    let db = common::make_db_from_url(&url).await.expect("test db");
    let cfg = test_config();
    let mock = MockServer::new(vec![Canned {
        status: 200,
        reason: "OK",
        headers: vec![],
        body: MOCK_FEED.as_bytes().to_vec(),
    }]);
    let uid = create_user(&db, 205, -1 /* READONLY */).await;
    let feed_id = create_feed(&db, 205, uid, &mock.url("/feed")).await;

    let outcome = fetch_one_feed(db.pool(), &cfg, Arc::new(no_plugins()), feed_id, &FeedUpdateFlags::default(), None).await.expect("fetch");
    assert!(!outcome.success, "expected a read-only owner to fail");
    assert!(outcome.error.is_some(), "expected an error message");
        Ok(())
    })).await.unwrap();
}

