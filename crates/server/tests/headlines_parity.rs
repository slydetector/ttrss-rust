//! Test parity: 1:1 port of
//! `tt-rss/tests_integration/FeedsHeadlinesIntegrationTest.php` (20 cases).
//!
//! Starting state matches the PHP suite: fresh per-test database with the
//! versioned `schema` + `baseline` + `seed` fixtures, plus the PHP
//! `setUp()`'s `Prefs::set(Prefs::FRESH_ARTICLE_MAX_AGE, 999999, 1, null)`
//! (a `ttrss_user_prefs2` row) and `DbTestCase::setUp`'s
//! `$_SESSION["uid"] = 1` — expressed here as the explicit
//! `owner_uid: 1` parameter.
//!
//! The PHP `Feeds::_get_headlines($params)` is ported to
//! `ttrss_server::headlines::get_headlines_query` (the same branch-for-
//! branch query builder the `Feeds::view` handler uses).

mod common;

use sqlx::Row;
use ttrss_core::db::Db;
use ttrss_server::headlines::{get_headlines_query, HeadlinesParams};

/// `DbTestCase::setUp`: `$_SESSION["uid"] = 1`.
const SESSION_UID: i64 = 1;

/// `DbTestCase::setUp` + the test class `setUp` on top of the harness pool.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> Db {
    let db = common::make_db(pool, options).await.expect("test db");
    // Prefs::set(Prefs::FRESH_ARTICLE_MAX_AGE, 999999, 1, null)
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'FRESH_ARTICLE_MAX_AGE', NULL, '999999')",
    )
    .execute(db.pool())
    .await
    .expect("set FRESH_ARTICLE_MAX_AGE");
    db
}

fn params(feed: i64) -> HeadlinesParams {
    HeadlinesParams::new(feed.to_string(), SESSION_UID)
}

fn row_ids(rows: &[sqlx::postgres::PgRow]) -> Vec<i32> {
    rows.iter().map(|r| r.try_get::<i32, _>("id").unwrap()).collect()
}

// ── Return structure ────────────────────────────────────────────────────────

// PHP: test_get_headlines_return_structure
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_return_structure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(-4)).await; // Feeds::FEED_ALL

    // The Rust HeadlinesQuery is the typed form of PHP's 9-element return
    // tuple: the row set, feed title / site URL / error / last updated
    // (strings), search words (array), first id, is_vfeed (bool) and the
    // query-error override (string, empty on success).
    let _ = q.article_rows.len();
    assert!(q.is_vfeed, "FEED_ALL is a virtual feed");
    assert!(
        q.query_error_override.is_empty(),
        "No query error on success"
    );
    assert!(q.search_words.is_empty());
    // feed_title / feed_site_url / last_error are Strings; last_updated is
    // Option<String> (PHP "" => None for virtual feeds).
    assert!(q.last_updated.is_none());
}

// ── Feed ID queries ─────────────────────────────────────────────────────────

// PHP: test_get_headlines_by_feed_id
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_by_feed_id(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(1)).await; // Hacker News

    assert_eq!("Hacker News", q.feed_title);
    assert!(q.article_rows.len() >= 3, "Feed 1 should have at least 3 articles");

    // All headlines should have required columns
    for row in &q.article_rows {
        for col in ["id", "title", "guid", "feed_id", "unread", "marked"] {
            assert!(row.try_column(col).is_ok(), "Missing column: {col}");
        }
    }
}

// PHP: test_get_headlines_by_feed_id_empty
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_by_feed_id_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Feed id 999 does not exist
    let q = get_headlines_query(&db, &params(999)).await;
    assert!(q.article_rows.is_empty());
}

// PHP: test_get_headlines_by_feed_id_different_owner
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_by_feed_id_different_owner(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Feed 1 exists but owner_uid=2 has no subscriptions
    let mut p = params(1);
    p.owner_uid = 2;
    let q = get_headlines_query(&db, &p).await;
    assert!(q.article_rows.is_empty(), "Owner 2 should see no headlines for feed 1");
}

// ── Virtual feeds ───────────────────────────────────────────────────────────

// PHP: test_get_headlines_feed_all
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_feed_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(-4)).await; // Feeds::FEED_ALL

    assert!(q.is_vfeed);
    assert!(
        q.article_rows.len() >= 11,
        "FEED_ALL should return all 11 seeded articles"
    );
}

// PHP: test_get_headlines_feed_starred
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_feed_starred(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(-1)).await; // Feeds::FEED_STARRED

    assert!(q.is_vfeed);
    // seed.sql marks articles 3, 8, 11 as marked=true
    assert_eq!(3, q.article_rows.len(), "Should return exactly 3 starred articles");

    for row in &q.article_rows {
        assert!(row.try_get::<bool, _>("marked").unwrap(), "Each starred headline should be marked");
    }
}

// PHP: test_get_headlines_feed_fresh
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_feed_fresh(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(-3)).await; // Feeds::FEED_FRESH

    assert!(
        !q.article_rows.is_empty(),
        "Should return some fresh articles"
    );

    for row in &q.article_rows {
        assert!(row.try_get::<bool, _>("unread").unwrap(), "Each fresh headline should be unread");
    }
}

// PHP: test_get_headlines_feed_recently_read
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_feed_recently_read(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Articles 3, 8, 11 are marked=true in seed but have last_read=NULL.
    // Use _catchup_by_id to set last_read = NOW() so they qualify as
    // "recently read".
    ttrss_server::article::catchup_by_id(
        db.pool(),
        &[3, 8, 11],
        ttrss_server::article::CATCHUP_MODE_MARK_AS_READ,
        SESSION_UID,
        None,
    )
    .await;

    let q = get_headlines_query(&db, &params(-6)).await; // Feeds::FEED_RECENTLY_READ

    // Virtual feed
    assert!(q.is_vfeed);

    // Feed title should be "Recently read"
    assert_eq!("Recently read", q.feed_title);

    // Exactly 3 articles should appear — the ones we just caught up
    assert_eq!(3, q.article_rows.len());

    // Verify the correct article IDs are present
    assert_eq!(vec![3, 8, 11], row_ids(&q.article_rows));

    // All should be read
    for row in &q.article_rows {
        assert!(!row.try_get::<bool, _>("unread").unwrap(), "Recently read articles should not be unread");
    }
}

// ── View modes ──────────────────────────────────────────────────────────────

// PHP: test_get_headlines_view_mode_unread
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_view_mode_unread(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(1); // Hacker News (3 articles: 2 unread, 1 read)
    p.view_mode = "unread".into();
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(2, q.article_rows.len(), "Feed 1 should have 2 unread articles");

    for row in &q.article_rows {
        assert!(row.try_get::<bool, _>("unread").unwrap(), "Each headline should be unread");
    }
}

// PHP: test_get_headlines_view_mode_marked
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_view_mode_marked(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(-4);
    p.view_mode = "marked".into();
    let q = get_headlines_query(&db, &p).await;

    // seed.sql has 3 marked articles (ids 3, 8, 11)
    assert_eq!(3, q.article_rows.len(), "Should return 3 marked articles");

    for row in &q.article_rows {
        assert!(row.try_get::<bool, _>("marked").unwrap(), "Each headline should be marked");
    }
}

// PHP: test_get_headlines_view_mode_published
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_view_mode_published(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(-4);
    p.view_mode = "published".into();
    let q = get_headlines_query(&db, &p).await;

    // None of the seeded articles are published
    assert!(q.article_rows.is_empty(), "No published articles in seed data");
}

// ── Pagination ──────────────────────────────────────────────────────────────

// PHP: test_get_headlines_limit
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_limit(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(-4);
    p.limit = 5;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(5, q.article_rows.len(), "Limit of 5 should return exactly 5 headlines");
}

// PHP: test_get_headlines_offset
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_offset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // First call: limit=5, offset=0 (page 1)
    let mut p = params(-4);
    p.limit = 5;
    let q1 = get_headlines_query(&db, &p).await;
    let ids_page1 = row_ids(&q1.article_rows);

    // Second call: limit=5, offset=5 (page 2)
    p.offset = 5;
    let q2 = get_headlines_query(&db, &p).await;
    let ids_page2 = row_ids(&q2.article_rows);

    assert_eq!(5, ids_page1.len());
    assert_eq!(5, ids_page2.len());

    // Page 1 and page 2 should not overlap
    let overlap = ids_page1.iter().filter(|id| ids_page2.contains(id)).count();
    assert_eq!(0, overlap, "Page 1 and page 2 should not overlap");

    // Together they should be 10 distinct articles
    let mut all: Vec<i32> = ids_page1;
    all.extend(ids_page2);
    let unique: std::collections::HashSet<_> = all.iter().copied().collect();
    assert_eq!(10, unique.len());
}

// ── Since ID ────────────────────────────────────────────────────────────────

// PHP: test_get_headlines_since_id
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_since_id(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(-4);
    p.since_id = 5;
    let q = get_headlines_query(&db, &p).await;

    // Should only return articles with id > 5
    for id in row_ids(&q.article_rows) {
        assert!(id > 5, "All returned articles should have id > 5");
    }
}

// ── Category view ───────────────────────────────────────────────────────────

// PHP: test_get_headlines_cat_view
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_cat_view(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category 1 = "Technology" (feeds 1, 2, 3)
    let mut p = params(1); // category id
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Technology", q.feed_title);
    // Category 1 has 3 feeds with 3+3+2 = 8 articles
    assert_eq!(8, q.article_rows.len(), "Category Technology should have 8 articles");
}

// PHP: test_get_headlines_cat_view_uncategorized
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_cat_view_uncategorized(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(0); // Feeds::CATEGORY_UNCATEGORIZED
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Uncategorized", q.feed_title);
    // All seeded feeds have a category, so uncategorized should be empty
    assert!(q.article_rows.is_empty());
}

// ── Metadata ────────────────────────────────────────────────────────────────

// PHP: test_get_headlines_metadata_feed
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_metadata_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(1)).await;

    assert_eq!("Hacker News", q.feed_title);
    assert_eq!("https://news.ycombinator.com", q.feed_site_url);
    assert!(q.last_error.is_empty());
}

// PHP: test_get_headlines_metadata_category
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_metadata_category(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let mut p = params(1);
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Technology", q.feed_title);
}

// ── Headline columns ────────────────────────────────────────────────────────

// PHP: test_get_headlines_columns
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_columns(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let q = get_headlines_query(&db, &params(1)).await;

    assert!(!q.article_rows.is_empty());

    let expected_columns = [
        "id", "date_entered", "guid", "title", "updated",
        "label_cache", "tag_cache", "always_display_enclosures",
        "site_url", "note", "num_comments", "comments",
        "int_id", "uuid", "lang", "hide_images",
        "unread", "feed_id", "marked", "published", "link",
        "last_read", "last_marked", "last_published",
        "content", "author", "score",
        "num_labels", "num_enclosures",
    ];

    for row in &q.article_rows {
        for col in &expected_columns {
            assert!(row.try_column(col).is_ok(), "Missing column: {col}");
        }
    }
}

// ── Feed with no articles for owner ─────────────────────────────────────────

// PHP: test_get_headlines_feed_not_subscribed
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_headlines_feed_not_subscribed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Feed 1 exists in the DB but owner_uid=2 has no ttrss_user_entries
    // for it
    let mut p = params(1);
    p.owner_uid = 2;
    let q = get_headlines_query(&db, &p).await;

    assert!(q.article_rows.is_empty());
    // Feed doesn't exist for owner_uid=2, so _get_title returns "Unknown feed"
    assert_eq!("Unknown feed (1)", q.feed_title);
}
