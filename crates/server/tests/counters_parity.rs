//! Test parity: 1:1 port of
//! `tt-rss/tests_integration/FeedsCountersIntegrationTest.php` (40 cases).
//!
//! Plus one Rust-only regression test for the `get_cats` `Some(ids)`
//! branch, which the PHP suite never exercises (see that section below).
//!
//! Starting state matches the PHP suite: fresh per-test database with the
//! versioned `schema` + `baseline` + `seed` fixtures, plus the PHP
//! `setUp()`'s `Prefs::set(Prefs::FRESH_ARTICLE_MAX_AGE, 999999, 1, null)`
//! (a `ttrss_user_prefs2` row) and `DbTestCase::setUp`'s
//! `$_SESSION["uid"] = 1` — expressed here as the explicit
//! `uid: 1` / `owner_uid` arguments on the Rust ports.
//!
//! The PHP `Feeds::_get_counters` / `Counters::{get_all, get_conditional,
//! get_labels}` are ported to `ttrss_server::counters::{get_counters,
//! get_all, get_conditional, get_labels}`.

mod common;

use serde_json::{json, Map, Value};
use ttrss_server::core::config::Config;
use ttrss_server::core::db::Db;
use ttrss_server::core::phpser::Value as PValue;
use ttrss_server::core::session::Session;
use ttrss_server::counters::{self, FeedOrTag};
use ttrss_server::session::Sess;
use ttrss_server::AppState;

/// `DbTestCase::setUp`: `$_SESSION["uid"] = 1`.
const SESSION_UID: i64 = 1;

/// No icons in the test environment (PHP's `_has_icon` uses the DiskCache,
/// which is empty in the fresh test DB) — a nonexistent directory gives the
/// same result.
const ICON_DIR: &str = "no-such-test-icon-dir";

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

/// Seed label ids for owner 1 (ttrss_labels2: 1 'test-label-1', 2 'test-label-2').
async fn seed_label_ids(db: &Db) -> Vec<i64> {
    let rows = sqlx::query_scalar::<_, i32>("SELECT id FROM ttrss_labels2 WHERE owner_uid = 1")
        .fetch_all(db.pool())
        .await
        .expect("label ids");
    rows.into_iter().map(|i| i as i64).collect()
}

/// PHP test filter: an entry with a positive int id plus feed-only keys.
fn is_feed_entry(v: &Value) -> bool {
    let Some(m) = v.as_object() else {
        return false;
    };
    let Some(id) = m.get("id").and_then(Value::as_i64) else {
        return false;
    };
    id > 0 && m.get("title").is_some() && m.get("ts").is_some()
}

/// PHP test filter: a label entry (negative id + description).
fn is_label_entry(v: &Value) -> bool {
    let Some(m) = v.as_object() else {
        return false;
    };
    let Some(id) = m.get("id").and_then(Value::as_i64) else {
        return false;
    };
    id < 0 && m.get("description").is_some()
}

// ── Return type ─────────────────────────────────────────────────────────────

// PHP: test_get_counters_returns_int
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_returns_int(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result =
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-4), false, false, None).await;
    assert!(result >= 0);
}

// ── Virtual feeds ───────────────────────────────────────────────────────────

// PHP: test_get_counters_feed_all
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // All 11 seeded articles belong to owner_uid=1
    assert_eq!(
        11,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-4), false, false, None).await
    );
}

// PHP: test_get_counters_feed_starred
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_starred(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // seed.sql: articles 3, 8, 11 are marked=true
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-1), false, false, None).await
    );
}

// PHP: test_get_counters_feed_starred_unread_only
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_starred_unread_only(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Starred articles: 3, 8, 11 — none are unread
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-1), false, true, None).await
    );
}

// PHP: test_get_counters_feed_published
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_published(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // No articles are published in seed data
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-2), false, false, None).await
    );
}

// PHP: test_get_counters_feed_fresh
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_fresh(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Fresh = unread + score >= 0 + within FRESH_ARTICLE_MAX_AGE.
    // setUp() sets FRESH_ARTICLE_MAX_AGE=999999 so all unread articles are
    // fresh: 1,2,4,5,6,7,9,10 = 8
    assert_eq!(
        8,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-3), false, false, None).await
    );
}

// PHP: test_get_counters_feed_recently_read
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_recently_read(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // FEED_RECENTLY_READ always returns 0
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-6), false, false, None).await
    );
}

// PHP: test_get_counters_feed_archived
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_feed_archived(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Archived = feed_id IS NULL — no articles have null feed_id
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(0), false, false, None).await
    );
}

// ── Regular feed IDs ────────────────────────────────────────────────────────

// PHP: test_get_counters_by_feed_id
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_by_feed_id(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Default (unread_only=false) returns TOTAL count.
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), false, false, None).await,
        "Feed 1 (Hacker News): articles 1,2,3 → total = 3"
    );
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(2), false, false, None).await,
        "Feed 2 (The Register): articles 4,5,6 → total = 3"
    );
    assert_eq!(
        2,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(3), false, false, None).await,
        "Feed 3 (Ars Technica): articles 7,8 → total = 2"
    );
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(4), false, false, None).await,
        "Feed 4 (MIT Tech Review): articles 9,10,11 → total = 3"
    );
}

// PHP: test_get_counters_by_feed_id_all
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_by_feed_id_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), false, false, None).await
    );
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(2), false, false, None).await
    );
    assert_eq!(
        2,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(3), false, false, None).await
    );
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(4), false, false, None).await
    );
}

// PHP: test_get_counters_by_feed_id_unread_only
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_by_feed_id_unread_only(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    assert_eq!(
        2,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), false, true, None).await
    );
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(2), false, true, None).await
    );
    assert_eq!(
        1,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(3), false, true, None).await
    );
    assert_eq!(
        2,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(4), false, true, None).await
    );
}

// PHP: test_get_counters_nonexistent_feed
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_nonexistent_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Feed 999 doesn't exist and has no user entries
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(999), false, false, None).await
    );
}

// ── Category view ───────────────────────────────────────────────────────────

// PHP: test_get_counters_category
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category 1 (Technology): feeds 1,2,3 → articles 1-8
    // Unread: 1,2,4,5,6,7 = 6
    assert_eq!(
        6,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), true, false, None).await
    );
}

// PHP: test_get_counters_category_all
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category queries always call _get_cat_unread() → returns unread count
    assert_eq!(
        6,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), true, false, None).await
    );
}

// PHP: test_get_counters_category_unread_only
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_unread_only(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // is_cat=true always returns unread via _get_cat_unread()
    assert_eq!(
        6,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), true, true, None).await
    );
}

// PHP: test_get_counters_category_science
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_science(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category 2 (Science & Nature): feed 4 → articles 9,10,11
    // Unread: 9,10 = 2
    assert_eq!(
        2,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(2), true, false, None).await
    );

    // Total: 3 articles
    assert_eq!(
        3,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(2), false, false, None).await
    );
}

// PHP: test_get_counters_category_uncategorized
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_uncategorized(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category 0 = uncategorized — all seeded feeds have a category
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(0), true, false, None).await
    );
}

// PHP: test_get_counters_category_special
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_special(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // CATEGORY_SPECIAL (-1) returns 0
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-1), true, false, None).await
    );
}

// ── Tag feed ────────────────────────────────────────────────────────────────

// PHP: test_get_counters_tag
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_tag(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Tags are stored in ttrss_tags table (not tag_cache).
    // Seed data only populates tag_cache strings, not ttrss_tags rows.
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Tag("rust"), false, false, None).await
    );
}

// PHP: test_get_counters_tag_nonexistent
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_tag_nonexistent(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    assert_eq!(
        0,
        counters::get_counters(
            &db,
            SESSION_UID,
            FeedOrTag::Tag("nonexistent-tag"),
            false,
            false,
            None
        )
        .await
    );
}

// PHP: test_get_counters_tag_all
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_tag_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // No tags in ttrss_tags table from seed data
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Tag("ai"), false, false, None).await
    );
}

// ── Label IDs ───────────────────────────────────────────────────────────────

// PHP: test_get_counters_label_id
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_label_id(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Insert label for uid=1
    let label_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_labels2 (owner_uid, caption) VALUES (1, 'test-counter-label') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .expect("insert label");

    // Assign label to article 1 (which is unread)
    sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES ($1, 1)")
        .bind(label_id as i64)
        .execute(db.pool())
        .await
        .expect("assign label");

    // Get the negative label reference ID
    let label_ref = ttrss_server::labels::label_to_feed_id(label_id as i64);

    // Count unread articles with this label
    assert_eq!(
        1,
        counters::get_counters(
            &db,
            SESSION_UID,
            FeedOrTag::Feed(label_ref),
            false,
            false,
            None
        )
        .await
    );

    // Count all articles with this label (PHP's _get_label_unread always
    // filters unread = true, so this is the same count)
    assert_eq!(
        1,
        counters::get_counters(
            &db,
            SESSION_UID,
            FeedOrTag::Feed(label_ref),
            false,
            false,
            None
        )
        .await
    );
    // NOTE: the PHP test's cleanup DELETEs are unnecessary — the harness
    // drops the per-test database.
}

// ── Owner UID ───────────────────────────────────────────────────────────────

// PHP: test_get_counters_different_owner
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_different_owner(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Feed 1 exists but owner_uid=2 has no user entries
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), false, false, Some(2)).await
    );
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-4), false, false, Some(2)).await
    );
}

// PHP: test_get_counters_category_different_owner
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_category_different_owner(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Category 1 has no entries for owner_uid=2
    assert_eq!(
        0,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(1), true, false, Some(2)).await
    );
}

// ── Cross-feed totals ───────────────────────────────────────────────────────

// PHP: test_get_counters_all_total
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_all_total(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // FEED_ALL for uid=1 should include all 11 articles
    assert_eq!(
        11,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-4), false, false, None).await
    );

    // Unread-only should be 8
    assert_eq!(
        8,
        counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(-4), false, true, None).await
    );
}

// PHP: test_get_counters_sum_per_feed
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_counters_sum_per_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    // Sum of individual feed counts should equal FEED_ALL total
    let mut total = 0;
    for i in 1..=4i64 {
        total +=
            counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(i), false, false, None).await;
    }
    assert_eq!(11, total);

    // Unread sum
    let mut unread_total = 0;
    for i in 1..=4i64 {
        unread_total +=
            counters::get_counters(&db, SESSION_UID, FeedOrTag::Feed(i), false, true, None).await;
    }
    assert_eq!(8, unread_total);
}

// ── Counters::get_feeds (via get_conditional) ───────────────────────────────

// PHP: test_counters_get_feeds_returns_array
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_feeds_returns_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        Some(&[1, 2]),
        None,
        std::path::Path::new(ICON_DIR),
    )
    .await;
    assert!(!result.is_empty());
}

// PHP: test_counters_get_feeds_contains_requested_feeds
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_feeds_contains_requested_feeds(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        Some(&[1, 2]),
        None,
        std::path::Path::new(ICON_DIR),
    )
    .await;

    // Filter to only feed entries (see is_feed_entry for the PHP condition)
    let feed_entries: Vec<&Value> = result.iter().filter(|v| is_feed_entry(v)).collect();
    assert_eq!(
        2,
        feed_entries.len(),
        "Should contain exactly feeds 1 and 2"
    );

    // Build lookup by id
    let by_id: std::collections::HashMap<i64, &serde_json::Map<String, Value>> = feed_entries
        .iter()
        .map(|v| {
            let m = v.as_object().unwrap();
            (m.get("id").and_then(Value::as_i64).unwrap(), m)
        })
        .collect();

    // Feed 1 (Hacker News): 2 unread, 1 marked
    let f1 = by_id[&1];
    assert_eq!(1, f1.get("id").and_then(Value::as_i64).unwrap());
    assert_eq!(
        2,
        f1.get("counter").and_then(Value::as_i64).unwrap(),
        "Feed 1 should have 2 unread"
    );
    assert_eq!(
        1,
        f1.get("markedcounter").and_then(Value::as_i64).unwrap(),
        "Feed 1 should have 1 starred"
    );
    assert_eq!(
        0,
        f1.get("publishedcounter").and_then(Value::as_i64).unwrap()
    );
    assert!(f1.get("error").is_some());
    assert!(f1.get("updated").is_some());

    // Feed 2 (The Register): 3 unread, 0 marked
    let f2 = by_id[&2];
    assert_eq!(
        3,
        f2.get("counter").and_then(Value::as_i64).unwrap(),
        "Feed 2 should have 3 unread"
    );
    assert_eq!(0, f2.get("markedcounter").and_then(Value::as_i64).unwrap());
}

// PHP: test_counters_get_feeds_empty_array
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_feeds_empty_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        Some(&[]),
        None,
        std::path::Path::new(ICON_DIR),
    )
    .await;

    let feed_entries: Vec<&Value> = result
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("id"))
                .and_then(Value::as_i64)
                .is_some_and(|id| id > 0)
        })
        .collect();
    assert!(
        feed_entries.is_empty(),
        "Empty feed_ids should yield no feed entries"
    );
}

// ── Rust-only regression: `get_cats` with a filtered cat list ──────────────
//
// The PHP suite never exercises the `Some(ids)` branch of
// `Counters::get_cats`: its `get_conditional` cases pass either an empty
// `feed_ids` (which skips the cat query entirely) or assert only on the feed
// entries, and `get_all` uses the static unfiltered cat query. That branch
// builds `fc.id IN ($2..$n+1)` plus a trailing `${n+2}` at runtime, and
// `get_cats` swallows query errors (`unwrap_or_default()`), so a malformed
// placeholder (the `${}$` bug fixed in July 2026) silently dropped every
// category row without failing any test. This pins the branch directly:
// feeds [1, 2] → `cats_of` → [Technology(1)] → the dynamic query must return
// exactly the Technology and uncategorized rows, with the correct counters.
//
// Seed fixtures: Technology (cat 1) holds feeds 1-3 → 6 unread (articles
// 1,2,4,5,6,7), 2 marked (3, 8), 0 published, no child cats; no seeded feed
// lacks a category; Science & Nature (cat 2) holds none of feeds 1-2.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_conditional_feed_ids_category_rows(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        Some(&[1, 2]),
        None,
        std::path::Path::new(ICON_DIR),
    )
    .await;

    let cat_entries: Vec<&Value> = result
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("kind"))
                .is_some_and(|k| k.as_str() == Some("cat"))
        })
        .collect();

    let by_id: std::collections::HashMap<i64, &Value> = cat_entries
        .iter()
        .map(|v| {
            let m = v.as_object().unwrap();
            (m.get("id").and_then(Value::as_i64).unwrap(), *v)
        })
        .collect();

    // Exactly the Labels entry, Technology, and uncategorized.
    assert_eq!(
        3,
        cat_entries.len(),
        "expected labels + Technology + uncategorized, got: {cat_entries:?}"
    );
    assert!(
        by_id.contains_key(&ttrss_server::cats::CAT_LABELS),
        "Labels category entry missing: {result:?}"
    );

    // Technology (cat 1) holds feeds 1-3: 6 unread, 2 marked, 0 published.
    let tech = by_id[&1];
    assert_eq!(
        6,
        tech.get("counter").and_then(Value::as_i64).unwrap(),
        "Technology unread: {tech:?}"
    );
    assert_eq!(
        2,
        tech.get("markedcounter").and_then(Value::as_i64).unwrap(),
        "Technology marked: {tech:?}"
    );
    assert_eq!(
        0,
        tech.get("publishedcounter")
            .and_then(Value::as_i64)
            .unwrap(),
        "Technology published: {tech:?}"
    );

    // Uncategorized: no seeded feed lacks a category.
    assert_eq!(
        0,
        by_id[&0].get("counter").and_then(Value::as_i64).unwrap(),
        "uncategorized: {:?}",
        by_id[&0]
    );

    // Science & Nature (cat 2) holds none of the requested feeds → absent.
    assert!(
        !by_id.contains_key(&2),
        "cat 2 must not appear for feeds [1, 2]: {cat_entries:?}"
    );
}
// ── Counters::get_all() ─────────────────────────────────────────────────────

// PHP: test_counters_get_all_returns_array
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_all_returns_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_all(&db, SESSION_UID, std::path::Path::new(ICON_DIR)).await;
    assert!(!result.is_empty());
}

// PHP: test_counters_get_all_contains_global_unread
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_all_contains_global_unread(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_all(&db, SESSION_UID, std::path::Path::new(ICON_DIR)).await;

    let global: Vec<&Value> = result
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("id"))
                .is_some_and(|id| id.as_str() == Some("global-unread"))
        })
        .collect();
    assert_eq!(1, global.len());
    let m = global[0].as_object().unwrap();
    assert!(m.get("counter").and_then(Value::as_i64).unwrap() >= 0);
}

// PHP: test_counters_get_all_contains_virtual_feeds
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_all_contains_virtual_feeds(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_all(&db, SESSION_UID, std::path::Path::new(ICON_DIR)).await;

    // Feeds::{FEED_ARCHIVED, FEED_STARRED, FEED_PUBLISHED, FEED_FRESH, FEED_ALL}
    let virtual_ids = [0, -1, -2, -3, -4];
    let virtual_entries: Vec<&Value> = result
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("id"))
                .and_then(Value::as_i64)
                .is_some_and(|id| virtual_ids.contains(&id))
        })
        .collect();
    assert!(
        virtual_entries.len() >= 5,
        "Should contain at least 5 virtual feed entries"
    );

    for entry in &virtual_entries {
        assert!(entry.as_object().unwrap().get("counter").is_some());
    }
}

// PHP: test_counters_get_all_contains_category_entries
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_all_contains_category_entries(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_all(&db, SESSION_UID, std::path::Path::new(ICON_DIR)).await;

    let cat_entries: Vec<&Value> = result
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("kind"))
                .is_some_and(|k| k.as_str() == Some("cat"))
        })
        .collect();
    // At least the Labels category + the 2 seed categories
    assert!(!cat_entries.is_empty());
}

// PHP: test_counters_get_all_contains_label_entries
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_all_contains_label_entries(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_all(&db, SESSION_UID, std::path::Path::new(ICON_DIR)).await;

    let label_entries: Vec<&Value> = result.iter().filter(|v| is_label_entry(v)).collect();
    // Seed data has 2 labels (test-label-1, test-label-2)
    assert_eq!(2, label_entries.len(), "Should contain seed label entries");
}

// ── Counters::get_labels() ──────────────────────────────────────────────────

// PHP: test_counters_get_labels_returns_array
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_labels_returns_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_labels(&db, SESSION_UID, None).await;
    assert!(!result.is_empty());
}

// PHP: test_counters_get_labels_with_ids
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_labels_with_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let label_ids = seed_label_ids(&db).await;
    assert!(!label_ids.is_empty());

    let result = counters::get_labels(&db, SESSION_UID, Some(&label_ids)).await;
    assert_eq!(
        label_ids.len(),
        result.len(),
        "Should return entries for the requested labels"
    );

    for entry in &result {
        let m = entry.as_object().unwrap();
        assert!(m.get("id").is_some());
        assert!(m.get("counter").is_some());
        assert!(m.get("auxcounter").is_some());
        assert!(m.get("description").is_some());
    }
}

// PHP: test_counters_get_labels_empty_array
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_labels_empty_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_labels(&db, SESSION_UID, Some(&[])).await;
    assert!(result.is_empty());
}

// PHP: test_counters_get_labels_nonexistent_ids
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_labels_nonexistent_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let result = counters::get_labels(&db, SESSION_UID, Some(&[99999, 99998])).await;
    assert!(result.is_empty());
}

// ── Counters::get_conditional with label_ids ────────────────────────────────

// PHP: test_counters_get_conditional_with_label_ids
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_conditional_with_label_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let label_ids = seed_label_ids(&db).await;
    assert!(!label_ids.is_empty());

    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        None,
        Some(&label_ids),
        std::path::Path::new(ICON_DIR),
    )
    .await;

    let label_entries: Vec<&Value> = result.iter().filter(|v| is_label_entry(v)).collect();
    assert_eq!(label_ids.len(), label_entries.len());
}

// PHP: test_counters_get_conditional_with_both_feed_and_label_ids
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_counters_get_conditional_with_both_feed_and_label_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let label_ids = seed_label_ids(&db).await;

    let result = counters::get_conditional(
        &db,
        SESSION_UID,
        Some(&[1, 2]),
        Some(&label_ids),
        std::path::Path::new(ICON_DIR),
    )
    .await;

    let feed_entries: Vec<&Value> = result.iter().filter(|v| is_feed_entry(v)).collect();
    let label_entries: Vec<&Value> = result.iter().filter(|v| is_label_entry(v)).collect();

    assert!(
        feed_entries.len() >= 2,
        "Should contain at least 2 requested feed entries"
    );
    assert_eq!(
        label_ids.len(),
        label_entries.len(),
        "Should contain label entries"
    );
}

// ── get_all_counters handler (RPC glue) ─────────────────────────────────────
//
// Rust-only: the PHP suite has no handler-level test for
// `RPC::getAllCounters` (its tests cover the `Counters::*` methods above).
// These pin the handler glue rewritten with the July 2026 `feed_ids[]` fix:
// the `feed_id_count`/`label_id_count` == -1 → null ("return all counters")
// contract, the `DISABLE_CONDITIONAL_COUNTERS` pref gate (PHP: `is_array
// ($feed_ids) && !Prefs::get(...)` → `Counters::get_all()`), the `seq` echo
// (`(int)` cast from string or numeric), and the label-subset wiring on the
// conditional path (`get_conditional($feed_ids, $label_ids)`).

/// Test-only config for the handler-level tests (mirrors `api_writes.rs`).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
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
    }
}

/// [`setup`] + the `AppState`/`Sess` the handler takes (session `uid = 1`,
/// the fixture owner).
async fn setup_state(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db = setup(pool, options).await;
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

/// The feed-entry ids in a counters response, sorted.
fn feed_ids_in(counters: &[Value]) -> Vec<i64> {
    let mut ids: Vec<i64> = counters
        .iter()
        .filter(|v| is_feed_entry(v))
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

/// The `kind: "cat"` entries in a counters response.
fn cat_entries(counters: &[Value]) -> Vec<&Value> {
    counters
        .iter()
        .filter(|v| {
            v.as_object()
                .and_then(|m| m.get("kind"))
                .is_some_and(|k| k.as_str() == Some("cat"))
        })
        .collect()
}

// Rust-only: `feed_ids[]` (string values, as the form parser produces) +
// `feed_id_count` → the conditional path: only the requested feeds, their
// category rows (Technology + uncategorized + Labels), all label entries
// (no `label_ids` param → PHP `get_labels(null)` = all labels), and the
// string `seq` echoed as an int.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_counters_handler_feed_ids_array(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup_state(pool, options).await;
    let mut extra = Map::new();
    extra.insert("seq".into(), json!("7"));
    extra.insert("feed_id_count".into(), json!(2));
    extra.insert("feed_ids".into(), json!(["1", "2"]));
    let v = counters::get_all_counters(&state, &sess, &extra).await;

    assert_eq!(v["seq"], json!(7), "seq: (int) cast of the string '7'");
    let c = v["counters"].as_array().expect("counters array");

    assert_eq!(vec![1, 2], feed_ids_in(c), "only the requested feeds");
    let f1 = c.iter().find(|v| v["id"] == json!(1)).unwrap();
    assert_eq!(f1["counter"], json!(2), "feed 1 unread");
    assert_eq!(f1["markedcounter"], json!(1), "feed 1 marked");
    let f2 = c.iter().find(|v| v["id"] == json!(2)).unwrap();
    assert_eq!(f2["counter"], json!(3), "feed 2 unread");

    // Categories of feeds 1-2: Labels + Technology (feeds 1-3: 6 unread) +
    // uncategorized (0) — not Science & Nature (feed 4).
    let cats = cat_entries(c);
    let mut cat_ids: Vec<i64> = cats.iter().map(|v| v["id"].as_i64().unwrap()).collect();
    cat_ids.sort_unstable();
    assert_eq!(
        vec![ttrss_server::cats::CAT_LABELS, 0, 1],
        cat_ids,
        "cat entries: {cats:?}"
    );

    // No label_ids param → PHP `get_labels(null)` → all seeded labels.
    let labels: Vec<&Value> = c.iter().filter(|v| is_label_entry(v)).collect();
    assert_eq!(2, labels.len(), "all seeded labels: {c:?}");
}

// Rust-only: absent `*_id_count` params → -1 → null → `Counters::get_all()`
// — every feed, every category, numeric `seq` echoed as-is.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_counters_handler_no_counts_returns_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup_state(pool, options).await;
    let mut extra = Map::new();
    extra.insert("seq".into(), json!(3));
    let v = counters::get_all_counters(&state, &sess, &extra).await;

    assert_eq!(v["seq"], json!(3));
    let c = v["counters"].as_array().expect("counters array");
    assert_eq!(vec![1, 2, 3, 4], feed_ids_in(c), "all seeded feeds");
    let mut cat_ids: Vec<i64> = cat_entries(c)
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    cat_ids.sort_unstable();
    assert_eq!(
        vec![ttrss_server::cats::CAT_LABELS, 0, 1, 2],
        cat_ids,
        "all categories + uncategorized + labels"
    );
}

// Rust-only: an explicit `feed_id_count: -1` overrides the supplied `feed_ids`
// (PHP: `$feed_id_count == -1 ? null : _param_to_int_array(...)`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_counters_handler_feed_id_count_minus_one(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup_state(pool, options).await;
    let mut extra = Map::new();
    extra.insert("feed_id_count".into(), json!(-1));
    extra.insert("feed_ids".into(), json!(["1", "2"]));
    let v = counters::get_all_counters(&state, &sess, &extra).await;

    let c = v["counters"].as_array().expect("counters array");
    assert_eq!(
        vec![1, 2, 3, 4],
        feed_ids_in(c),
        "count -1 → null → all feeds"
    );
}

// Rust-only: `DISABLE_CONDITIONAL_COUNTERS` → `Counters::get_all()` even when
// feed ids were supplied (PHP: `is_array($feed_ids) && !Prefs::get(...)`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_counters_handler_disable_conditional(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup_state(pool, options).await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'DISABLE_CONDITIONAL_COUNTERS', NULL, '1')",
    )
    .execute(state.db.pool())
    .await
    .expect("set DISABLE_CONDITIONAL_COUNTERS");

    let mut extra = Map::new();
    extra.insert("feed_id_count".into(), json!(2));
    extra.insert("feed_ids".into(), json!(["1", "2"]));
    let v = counters::get_all_counters(&state, &sess, &extra).await;

    let c = v["counters"].as_array().expect("counters array");
    assert_eq!(
        vec![1, 2, 3, 4],
        feed_ids_in(c),
        "pref gate → get_all() ignores the feed filter"
    );
    let mut cat_ids: Vec<i64> = cat_entries(c)
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    cat_ids.sort_unstable();
    assert_eq!(
        vec![ttrss_server::cats::CAT_LABELS, 0, 1, 2],
        cat_ids,
        "get_all() → all categories"
    );
}

// Rust-only: `label_ids` on the conditional path → only the requested label
// entries (PHP `get_conditional($feed_ids, $label_ids)` → `get_labels($label_ids)`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_counters_handler_label_ids_subset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup_state(pool, options).await;
    let mut extra = Map::new();
    extra.insert("feed_id_count".into(), json!(2));
    extra.insert("feed_ids".into(), json!(["1", "2"]));
    extra.insert("label_id_count".into(), json!(1));
    extra.insert("label_ids".into(), json!("1"));
    let v = counters::get_all_counters(&state, &sess, &extra).await;

    let c = v["counters"].as_array().expect("counters array");
    let labels: Vec<&Value> = c.iter().filter(|v| is_label_entry(v)).collect();
    assert_eq!(1, labels.len(), "only the requested label: {c:?}");
    assert_eq!(
        ttrss_server::labels::label_to_feed_id(1),
        labels[0]["id"].as_i64().unwrap()
    );
}
