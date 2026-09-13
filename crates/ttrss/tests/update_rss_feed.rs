//! M6b.2b — the DB-backed feed-update pipeline (`ttrss::updates`), mirroring
//! `tt-rss/tests_integration/UpdateRssFeedTest.php`.
//!
//! The Rust `update_rss_feed` takes the feed XML as input (the network fetch is
//! M6c), so these replay the PHP suite's recorded mock feed
//! (`tt-rss/tests_integration/feed.xml` — one Atom article, "x-21may2020")
//! against the versioned schema + baseline + seed fixtures, asserting the
//! permission gate, the article upsert, the feed metadata stamp, and dedup.

mod common;

use sqlx::pool::PoolOptions;
use sqlx::postgres::PgConnectOptions;
use sqlx::Row;
use ttrss::core::db::Db;
use ttrss::updates::update_rss_feed;

/// The recorded mock feed (a single Atom entry, "x-21may2020").
const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// The PHP suite's invalid-XML string.
const BAD_FEED: &str = "<invalid-xml><unclosed>";

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

/// `ttrss_entries` count for a feed (PHP: entries ⋈ user_entries by ref_id).
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

/// The feed's `last_successful_update` (non-null when a successful update ran).
async fn last_successful_update_is_set(db: &Db, feed_id: i32) -> bool {
    sqlx::query_scalar("SELECT last_successful_update IS NOT NULL FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_successful_update")
}

/// The feed's `last_error` (empty after a successful update).
async fn last_error(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT last_error FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_error")
}

// PHP: testNonexistentFeedReturnsFalse
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_nonexistent_feed_returns_false(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    assert!(!update_rss_feed(db.pool(), 9999, MOCK_FEED)
        .await
        .expect("update"));
}

// PHP: testReadonlyUserCannotUpdateFeed
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_readonly_user_cannot_update_feed(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let readonly_uid = create_user(&db, 100, -1 /* READONLY */).await;
    let feed_id = create_feed(&db, 100, readonly_uid, "http://example.com/feed1.xml").await;

    assert!(!update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
}

// PHP: testSuccessfulUpdateReturnsTrue
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_successful_update_returns_true(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 101, 0 /* USER */).await;
    let feed_id = create_feed(&db, 101, uid, "http://example.com/feed2.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    assert_eq!(entry_count(&db, feed_id).await, 1);
}

// PHP: testFeedMetadataUpdated
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_feed_metadata_updated(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 102, 0).await;
    let feed_id = create_feed(&db, 102, uid, "http://example.com/feed3.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    assert!(last_successful_update_is_set(&db, feed_id).await);
    assert_eq!(last_error(&db, feed_id).await, "");
}

// PHP: testArticleFieldsStoredCorrectly
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_article_fields_stored_correctly(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 103, 0).await;
    let feed_id = create_feed(&db, 103, uid, "http://example.com/feed4.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));

    let row = sqlx::query(
        "SELECT e.title, e.link, e.updated, e.num_comments \
         FROM ttrss_entries e JOIN ttrss_user_entries ue ON e.id = ue.ref_id \
         WHERE ue.feed_id = $1 LIMIT 1",
    )
    .bind(feed_id)
    .fetch_one(db.pool())
    .await
    .expect("entry row");

    let title: &str = row.get(0);
    let link: &str = row.get(1);
    let updated: Option<chrono::NaiveDateTime> = row.get(2);
    let num_comments: i32 = row.get(3);

    assert_eq!(title, "x-21may2020");
    assert!(
        link.contains("fakecake.org"),
        "link should contain feed domain: {link}"
    );
    assert!(
        link.contains("clouds.png"),
        "link should contain article path: {link}"
    );
    assert!(updated.is_some());
    assert_eq!(num_comments, 0);
}

// PHP: testSecondUpdateDoesNotCreateDuplicate
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_second_update_does_not_create_duplicate(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 104, 0).await;
    let feed_id = create_feed(&db, 104, uid, "http://example.com/feed5.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update #1"));
    assert_eq!(entry_count(&db, feed_id).await, 1);

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update #2"));
    assert_eq!(entry_count(&db, feed_id).await, 1); // still 1, not 2
}

// PHP: testParseErrorReturnsFalse
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_parse_error_returns_false(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 105, 0).await;
    let feed_id = create_feed(&db, 105, uid, "http://example.com/feed6.xml").await;

    assert!(!update_rss_feed(db.pool(), feed_id, BAD_FEED)
        .await
        .expect("update"));
    assert!(!last_error(&db, feed_id).await.is_empty());
}
