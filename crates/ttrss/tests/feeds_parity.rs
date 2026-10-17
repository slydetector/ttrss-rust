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

use test_db::template::with_test_db;
use ttrss::feeds;
use ttrss::i18n::{I18N, I18nCache, I18nCtx};

/// PHP `Feeds::*` constants (mirrors `tt-rss/classes/Feeds.php`).
const FEED_ARCHIVED: i32 = 0;
const FEED_STARRED: i32 = -1;
const FEED_PUBLISHED: i32 = -2;
const FEED_FRESH: i32 = -3;
const FEED_ALL: i32 = -4;
const FEED_RECENTLY_READ: i32 = -6;
const FEED_ERROR: i32 = -7;
const CATEGORY_UNCATEGORIZED: i32 = 0;
const CATEGORY_SPECIAL: i32 = -1;
const CATEGORY_LABELS: i32 = -2;

// ── FeedsTest (functional: no seed data required) ───────────────────────────

/// PHP: test_get_title_special_feeds (functional + integration).
#[tokio::test]
async fn test_get_title_special_feeds() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            let uid = 1i32;
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
        })
    })
    .await
    .unwrap();
}

/// Rust-only (the frozen PHP suite has no `_get_title` locale test): the port
/// must carry the request's locale. `test_get_title_special_feeds` runs with no
/// locale (the `__()` falls back to English); this one sets `de_DE` and checks
/// the virtual-feed titles are actually translated, matching PHP `Feeds.php:1151-1161`.
#[tokio::test]
async fn test_get_title_special_feeds_translated() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            let cache = std::sync::Arc::new(I18nCache::new(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join("tt-rss")
                    .join("locale"),
            ));
            let ctx = I18nCtx {
                locale: Some("de_DE".into()),
                cache,
            };
            let title = I18N
                .scope(ctx, async {
                    feeds::feed_title(db.pool(), FEED_STARRED, 1).await
                })
                .await;
            assert_eq!(
                "Markierte Artikel", title,
                "FEED_STARRED must carry the request's locale (de_DE)"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_error_feed (functional + integration).
#[tokio::test]
async fn test_get_title_error_feed() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            // FEED_ERROR (-7) is < 0 and >= LABEL_BASE_INDEX (-1024): the final else
            // branch returns the id as a string.
            assert_eq!("-7", feeds::feed_title(db.pool(), FEED_ERROR, 1).await);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── FeedsIntegrationTest (DB-backed) ─────────────────────────────────────────

/// PHP: test_get_title_numeric_feed_id_existing.
#[tokio::test]
async fn test_get_title_numeric_feed_id_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            let uid = 1i32;
            assert_eq!("Hacker News", feeds::feed_title(db.pool(), 1, uid).await);
            assert_eq!("The Register", feeds::feed_title(db.pool(), 2, uid).await);
            assert_eq!("Ars Technica", feeds::feed_title(db.pool(), 3, uid).await);
            assert_eq!(
                "MIT Tech Review",
                feeds::feed_title(db.pool(), 4, uid).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_numeric_feed_id_missing.
#[tokio::test]
async fn test_get_title_numeric_feed_id_missing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            // Feed id 999 does not exist for owner 1.
            assert_eq!(
                "Unknown feed (999)",
                feeds::feed_title(db.pool(), 999, 1).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_label_id_existing.
#[tokio::test]
async fn test_get_title_label_id_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            let uid = 1i32;
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
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_label_id_missing.
#[tokio::test]
async fn test_get_title_label_id_missing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            // Label id 9999 does not exist; its reference is < LABEL_BASE_INDEX.
            let ref_id = ttrss::labels::label_to_feed_id(9999);
            assert_eq!(
                "Unknown label (9999)",
                feeds::feed_title(db.pool(), ref_id, 1).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_category_special.
#[tokio::test]
async fn test_get_title_category_special() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            assert_eq!(
                "Special",
                feeds::category_title(db.pool(), CATEGORY_SPECIAL, 1).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_category_uncategorized.
#[tokio::test]
async fn test_get_title_category_uncategorized() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            assert_eq!(
                "Uncategorized",
                feeds::category_title(db.pool(), CATEGORY_UNCATEGORIZED, 1).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_category_labels.
#[tokio::test]
async fn test_get_title_category_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            assert_eq!(
                "Labels",
                feeds::category_title(db.pool(), CATEGORY_LABELS, 1).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_category_existing.
#[tokio::test]
async fn test_get_title_category_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            let uid = 1i32;
            assert_eq!("Technology", feeds::category_title(db.pool(), 1, uid).await);
            assert_eq!(
                "Science & Nature",
                feeds::category_title(db.pool(), 2, uid).await
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_category_missing.
#[tokio::test]
async fn test_get_title_category_missing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            // Category id 999 does not exist for owner 1.
            assert_eq!("UNKNOWN", feeds::category_title(db.pool(), 999, 1).await);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_title_different_owner_uid.
#[tokio::test]
async fn test_get_title_different_owner_uid() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_ref(&url).await;
            // seed data is for owner 1; owner 2 has no categories.
            assert_eq!("UNKNOWN", feeds::category_title(db.pool(), 1, 2).await);
            Ok(())
        })
    })
    .await
    .unwrap();
}
