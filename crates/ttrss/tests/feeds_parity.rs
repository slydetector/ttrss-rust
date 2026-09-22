//! Test parity: 1:1 port of
//! `tt-rss/tests_functional/FeedsTest.php` (3 cases) and
//! `tt-rss/tests_integration/FeedsIntegrationTest.php` (13 cases) — the
//! `Feeds::_get_title` / `Feeds::_get_cat_title` feed-title resolution.
//!
//! The PHP `Feeds::_get_title` is ported to `ttrss::feeds::feed_title`
//! (the non-cat branch: the six virtual feeds, labels, and real feeds) and
//! `category_title` (the `cat=true` branch: the special categories + real
//! categories). The integration cases run on the fresh per-test database with
//! the versioned `schema` + `baseline` + `seed` fixtures (feeds 1-4, labels
//! 1-2, categories 1-2 for owner 1).
//!
//! NOTE: the PHP `_get_title($id, ...)` accepts a raw string id, so a
//! non-numeric id falls through unchanged (`test_get_title_non_numeric_id`).
//! The Rust `feed_title` takes an `i64` (the caller parses the id), so that
//! case is handled by the id parser at the handler layer and has no
//! `feed_title` equivalent.

mod common;

use ttrss::core::db::Db;
use ttrss::feeds;
use test_db::template::with_test_db;

/// PHP `Feeds::*` constants (mirrors `tt-rss/classes/Feeds.php`).
const FEED_ARCHIVED: i64 = 0;
const FEED_STARRED: i64 = -1;
const FEED_PUBLISHED: i64 = -2;
const FEED_FRESH: i64 = -3;
const FEED_ALL: i64 = -4;
const FEED_RECENTLY_READ: i64 = -6;
const FEED_ERROR: i64 = -7;
const CATEGORY_UNCATEGORIZED: i64 = 0;
const CATEGORY_SPECIAL: i64 = -1;
const CATEGORY_LABELS: i64 = -2;

// ── FeedsTest (functional: no seed data required) ───────────────────────────

/// PHP: test_get_title_special_feeds (functional + integration).
#[tokio::test]
async fn test_get_title_special_feeds() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        let uid = 1;
        assert_eq!(
            "Starred articles",
            feeds::feed_title(db.pool(), FEED_STARRED, uid).await
        );
        assert_eq!(
            "Published articles",
            feeds::feed_title(db.pool(), FEED_PUBLISHED, uid).await
        );
        assert_eq!(
            "Fresh articles",
            feeds::feed_title(db.pool(), FEED_FRESH, uid).await
        );
        assert_eq!(
            "All articles",
            feeds::feed_title(db.pool(), FEED_ALL, uid).await
        );
        assert_eq!(
            "Archived articles",
            feeds::feed_title(db.pool(), FEED_ARCHIVED, uid).await
        );
        assert_eq!(
            "Recently read",
            feeds::feed_title(db.pool(), FEED_RECENTLY_READ, uid).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_error_feed (functional + integration).
#[tokio::test]
async fn test_get_title_error_feed() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        // FEED_ERROR (-7) is < 0 and >= LABEL_BASE_INDEX (-1024): the final else
        // branch returns the id as a string.
        assert_eq!("-7", feeds::feed_title(db.pool(), FEED_ERROR, 1).await);
        Ok(())
    })).await.unwrap();
}

// ── FeedsIntegrationTest (DB-backed) ─────────────────────────────────────────

/// PHP: test_get_title_numeric_feed_id_existing.
#[tokio::test]
async fn test_get_title_numeric_feed_id_existing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        let uid = 1;
        assert_eq!("Hacker News", feeds::feed_title(db.pool(), 1, uid).await);
        assert_eq!("The Register", feeds::feed_title(db.pool(), 2, uid).await);
        assert_eq!("Ars Technica", feeds::feed_title(db.pool(), 3, uid).await);
        assert_eq!(
            "MIT Tech Review",
            feeds::feed_title(db.pool(), 4, uid).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_numeric_feed_id_missing.
#[tokio::test]
async fn test_get_title_numeric_feed_id_missing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        // Feed id 999 does not exist for owner 1.
        assert_eq!(
            "Unknown feed (999)",
            feeds::feed_title(db.pool(), 999, 1).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_label_id_existing.
#[tokio::test]
async fn test_get_title_label_id_existing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        let uid = 1;
        // Label reference IDs must be < LABEL_BASE_INDEX (-1024); the label id is
        // recovered from the reference via `feed_to_label_id`.
        let ref_id_1 = ttrss::labels::label_to_feed_id(1);
        assert_eq!(
            "test-label-1",
            feeds::feed_title(db.pool(), ref_id_1, uid).await
        );
        let ref_id_2 = ttrss::labels::label_to_feed_id(2);
        assert_eq!(
            "test-label-2",
            feeds::feed_title(db.pool(), ref_id_2, uid).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_label_id_missing.
#[tokio::test]
async fn test_get_title_label_id_missing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        // Label id 9999 does not exist; its reference is < LABEL_BASE_INDEX.
        let ref_id = ttrss::labels::label_to_feed_id(9999);
        assert_eq!(
            "Unknown label (9999)",
            feeds::feed_title(db.pool(), ref_id, 1).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_category_special.
#[tokio::test]
async fn test_get_title_category_special() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        assert_eq!(
            "Special",
            feeds::category_title(db.pool(), CATEGORY_SPECIAL, 1).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_category_uncategorized.
#[tokio::test]
async fn test_get_title_category_uncategorized() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        assert_eq!(
            "Uncategorized",
            feeds::category_title(db.pool(), CATEGORY_UNCATEGORIZED, 1).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_category_labels.
#[tokio::test]
async fn test_get_title_category_labels() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        assert_eq!(
            "Labels",
            feeds::category_title(db.pool(), CATEGORY_LABELS, 1).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_category_existing.
#[tokio::test]
async fn test_get_title_category_existing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        let uid = 1;
        assert_eq!("Technology", feeds::category_title(db.pool(), 1, uid).await);
        assert_eq!(
            "Science & Nature",
            feeds::category_title(db.pool(), 2, uid).await
        );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_category_missing.
#[tokio::test]
async fn test_get_title_category_missing() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        // Category id 999 does not exist for owner 1.
        assert_eq!("UNKNOWN", feeds::category_title(db.pool(), 999, 1).await);
        Ok(())
    })).await.unwrap();
}

/// PHP: test_get_title_different_owner_uid.
#[tokio::test]
async fn test_get_title_different_owner_uid() {
    let _ = with_test_db(|url| Box::pin(async move {
        let db = common::make_db_ref(&url).await;
        // seed data is for owner 1; owner 2 has no categories.
        assert_eq!("UNKNOWN", feeds::category_title(db.pool(), 1, 2).await);
        Ok(())
    })).await.unwrap();
}
