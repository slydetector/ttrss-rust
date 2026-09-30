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
//! `ttrss::headlines::get_headlines_query` (the same branch-for-
//! branch query builder the `Feeds::view` handler uses).

mod common;

use sqlx::Row;
use ttrss::core::db::Db;
use ttrss::headlines::{get_headlines_query, HeadlinesParams};
use test_db::template::with_test_db;

/// `DbTestCase::setUp`: `$_SESSION["uid"] = 1`.
const SESSION_UID: i64 = 1;

/// `DbTestCase::setUp` + the test class `setUp` on top of the harness pool.
async fn setup(
    url: &str,
) -> Db {
    let db = common::make_db_from_url(url).await.expect("test db");
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
    rows.iter()
        .map(|r| r.try_get::<i32, _>("id").unwrap())
        .collect()
}

// ── Return structure ────────────────────────────────────────────────────────

// PHP: test_get_headlines_return_structure
#[tokio::test]
async fn test_get_headlines_return_structure() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
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
        Ok(())
    })).await.unwrap();
}

// ── Feed ID queries ─────────────────────────────────────────────────────────

// PHP: test_get_headlines_by_feed_id
#[tokio::test]
async fn test_get_headlines_by_feed_id() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(1)).await; // Hacker News

    assert_eq!("Hacker News", q.feed_title);
    assert!(
        q.article_rows.len() >= 3,
        "Feed 1 should have at least 3 articles"
    );

    // All headlines should have required columns
    for row in &q.article_rows {
        for col in ["id", "title", "guid", "feed_id", "unread", "marked"] {
            assert!(row.try_column(col).is_ok(), "Missing column: {col}");
        }
    }
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_by_feed_id_empty
#[tokio::test]
async fn test_get_headlines_by_feed_id_empty() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Feed id 999 does not exist
    let q = get_headlines_query(&db, &params(999)).await;
    assert!(q.article_rows.is_empty());
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_by_feed_id_different_owner
#[tokio::test]
async fn test_get_headlines_by_feed_id_different_owner() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Feed 1 exists but owner_uid=2 has no subscriptions
    let mut p = params(1);
    p.owner_uid = 2;
    let q = get_headlines_query(&db, &p).await;
    assert!(
        q.article_rows.is_empty(),
        "Owner 2 should see no headlines for feed 1"
    );
        Ok(())
    })).await.unwrap();
}

// ── Virtual feeds ───────────────────────────────────────────────────────────

// PHP: test_get_headlines_feed_all
#[tokio::test]
async fn test_get_headlines_feed_all() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(-4)).await; // Feeds::FEED_ALL

    assert!(q.is_vfeed);
    assert!(
        q.article_rows.len() >= 11,
        "FEED_ALL should return all 11 seeded articles"
    );
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_feed_starred
#[tokio::test]
async fn test_get_headlines_feed_starred() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(-1)).await; // Feeds::FEED_STARRED

    assert!(q.is_vfeed);
    // seed.sql marks articles 3, 8, 11 as marked=true
    assert_eq!(
        3,
        q.article_rows.len(),
        "Should return exactly 3 starred articles"
    );

    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("marked").unwrap(),
            "Each starred headline should be marked"
        );
    }
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_feed_fresh
#[tokio::test]
async fn test_get_headlines_feed_fresh() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(-3)).await; // Feeds::FEED_FRESH

    assert!(
        !q.article_rows.is_empty(),
        "Should return some fresh articles"
    );

    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("unread").unwrap(),
            "Each fresh headline should be unread"
        );
    }
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_feed_recently_read
#[tokio::test]
async fn test_get_headlines_feed_recently_read() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Articles 3, 8, 11 are marked=true in seed but have last_read=NULL.
    // Use _catchup_by_id to set last_read = NOW() so they qualify as
    // "recently read".
    ttrss::article::catchup_by_id(
        db.pool(),
        &[3, 8, 11],
        ttrss::article::CATCHUP_MODE_MARK_AS_READ,
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
        assert!(
            !row.try_get::<bool, _>("unread").unwrap(),
            "Recently read articles should not be unread"
        );
    }
        Ok(())
    })).await.unwrap();
}

// ── View modes ──────────────────────────────────────────────────────────────

// PHP: test_get_headlines_view_mode_unread
#[tokio::test]
async fn test_get_headlines_view_mode_unread() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(1); // Hacker News (3 articles: 2 unread, 1 read)
    p.view_mode = "unread".into();
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(
        2,
        q.article_rows.len(),
        "Feed 1 should have 2 unread articles"
    );

    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("unread").unwrap(),
            "Each headline should be unread"
        );
    }
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_view_mode_marked
#[tokio::test]
async fn test_get_headlines_view_mode_marked() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(-4);
    p.view_mode = "marked".into();
    let q = get_headlines_query(&db, &p).await;

    // seed.sql has 3 marked articles (ids 3, 8, 11)
    assert_eq!(3, q.article_rows.len(), "Should return 3 marked articles");

    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("marked").unwrap(),
            "Each headline should be marked"
        );
    }
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_view_mode_published
#[tokio::test]
async fn test_get_headlines_view_mode_published() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(-4);
    p.view_mode = "published".into();
    let q = get_headlines_query(&db, &p).await;

    // None of the seeded articles are published
    assert!(
        q.article_rows.is_empty(),
        "No published articles in seed data"
    );
        Ok(())
    })).await.unwrap();
}

// Rust-only. `adaptive` is the default view mode but the PHP suite exercises
// the explicit "unread"/"marked"/"published" modes, not adaptive. The ported
// behavior: a concrete feed with any unread is scoped to unread; with no
// unread it falls back to all articles (Feeds.php `$view_mode == "adaptive"`).
#[tokio::test]
async fn test_get_headlines_view_mode_adaptive_scopes_to_unread() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(1); // Hacker News (3 articles: 2 unread, 1 read)
    p.view_mode = "adaptive".into();
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(
        2,
        q.article_rows.len(),
        "adaptive should show only the 2 unread articles"
    );
    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("unread").unwrap(),
            "each adaptive headline should be unread"
        );
    }
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_headlines_view_mode_adaptive_falls_back_to_all() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Mark every article in feed 1 read so there is no unread to scope to.
    let ids: Vec<i64> = sqlx::query_scalar::<_, i32>(
        "SELECT e.id FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON e.id = ue.ref_id \
         WHERE ue.feed_id = 1 AND ue.owner_uid = 1",
    )
    .fetch_all(db.pool())
    .await
    .expect("feed 1 article ids")
    .into_iter()
    .map(|n| n as i64)
    .collect();
    ttrss::article::catchup_by_id(
        db.pool(),
        &ids,
        ttrss::article::CATCHUP_MODE_MARK_AS_READ,
        SESSION_UID,
        None,
    )
    .await;

    let mut p = params(1);
    p.view_mode = "adaptive".into();
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(
        ids.len(),
        q.article_rows.len(),
        "adaptive with no unread should fall back to all articles"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_headlines_view_mode_adaptive_category() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(1); // category 1 = Technology (feeds 1, 2, 3)
    p.cat_view = true;
    p.view_mode = "adaptive".into();
    let q = get_headlines_query(&db, &p).await;

    // Independent oracle: the unread articles in category 1.
    let expected_unread: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_user_entries ue \
         JOIN ttrss_feeds f ON f.id = ue.feed_id \
         WHERE f.cat_id = 1 AND ue.unread AND ue.owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("cat 1 unread");
    assert!(expected_unread > 0, "category 1 should have unread in the seed");
    assert_eq!(
        expected_unread as usize,
        q.article_rows.len(),
        "adaptive category view should show exactly the unread articles"
    );
    for row in &q.article_rows {
        assert!(
            row.try_get::<bool, _>("unread").unwrap(),
            "each adaptive category headline should be unread"
        );
    }
        Ok(())
    })).await.unwrap();
}

// ── Pagination ──────────────────────────────────────────────────────────────

// PHP: test_get_headlines_limit
#[tokio::test]
async fn test_get_headlines_limit() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(-4);
    p.limit = 5;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!(
        5,
        q.article_rows.len(),
        "Limit of 5 should return exactly 5 headlines"
    );
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_offset
#[tokio::test]
async fn test_get_headlines_offset() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
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
        Ok(())
    })).await.unwrap();
}

// ── Since ID ────────────────────────────────────────────────────────────────

// PHP: test_get_headlines_since_id
#[tokio::test]
async fn test_get_headlines_since_id() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(-4);
    p.since_id = 5;
    let q = get_headlines_query(&db, &p).await;

    // Should only return articles with id > 5
    for id in row_ids(&q.article_rows) {
        assert!(id > 5, "All returned articles should have id > 5");
    }
        Ok(())
    })).await.unwrap();
}

// ── Category view ───────────────────────────────────────────────────────────

// PHP: test_get_headlines_cat_view
#[tokio::test]
async fn test_get_headlines_cat_view() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Category 1 = "Technology" (feeds 1, 2, 3)
    let mut p = params(1); // category id
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Technology", q.feed_title);
    // Category 1 has 3 feeds with 3+3+2 = 8 articles
    assert_eq!(
        8,
        q.article_rows.len(),
        "Category Technology should have 8 articles"
    );
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_cat_view_uncategorized
#[tokio::test]
async fn test_get_headlines_cat_view_uncategorized() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(0); // Feeds::CATEGORY_UNCATEGORIZED
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Uncategorized", q.feed_title);
    // All seeded feeds have a category, so uncategorized should be empty
    assert!(q.article_rows.is_empty());
        Ok(())
    })).await.unwrap();
}

// ── Metadata ────────────────────────────────────────────────────────────────

// PHP: test_get_headlines_metadata_feed
#[tokio::test]
async fn test_get_headlines_metadata_feed() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(1)).await;

    assert_eq!("Hacker News", q.feed_title);
    assert_eq!("https://news.ycombinator.com", q.feed_site_url);
    assert!(q.last_error.is_empty());
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_headlines_metadata_category
#[tokio::test]
async fn test_get_headlines_metadata_category() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let mut p = params(1);
    p.cat_view = true;
    let q = get_headlines_query(&db, &p).await;

    assert_eq!("Technology", q.feed_title);
        Ok(())
    })).await.unwrap();
}

// ── Headline columns ────────────────────────────────────────────────────────

// PHP: test_get_headlines_columns
#[tokio::test]
async fn test_get_headlines_columns() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let q = get_headlines_query(&db, &params(1)).await;

    assert!(!q.article_rows.is_empty());

    let expected_columns = [
        "id",
        "date_entered",
        "guid",
        "title",
        "updated",
        "label_cache",
        "tag_cache",
        "always_display_enclosures",
        "site_url",
        "note",
        "num_comments",
        "comments",
        "int_id",
        "uuid",
        "lang",
        "hide_images",
        "unread",
        "feed_id",
        "marked",
        "published",
        "link",
        "last_read",
        "last_marked",
        "last_published",
        "content",
        "author",
        "score",
        "num_labels",
        "num_enclosures",
    ];

    for row in &q.article_rows {
        for col in &expected_columns {
            assert!(row.try_column(col).is_ok(), "Missing column: {col}");
        }
    }
        Ok(())
    })).await.unwrap();
}

// ── Feed with no articles for owner ─────────────────────────────────────────

// PHP: test_get_headlines_feed_not_subscribed
#[tokio::test]
async fn test_get_headlines_feed_not_subscribed() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Feed 1 exists in the DB but owner_uid=2 has no ttrss_user_entries
    // for it
    let mut p = params(1);
    p.owner_uid = 2;
    let q = get_headlines_query(&db, &p).await;

    assert!(q.article_rows.is_empty());
    // Feed doesn't exist for owner_uid=2, so _get_title returns "Unknown feed"
    assert_eq!("Unknown feed (1)", q.feed_title);
        Ok(())
    })).await.unwrap();
}
