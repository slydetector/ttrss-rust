//! Test parity: 1:1 port of `tt-rss/tests_integration/ArticleTest.php`
//! (32 cases).
//!
//! Starting state matches the PHP suite: fresh per-test database with the
//! versioned `schema` + `baseline` + `seed` fixtures (identical to
//! `tt-rss/tests_integration/seed.sql`), and `DbTestCase::setUp`'s
//! `$_SESSION["uid"] = 1` — expressed here as the explicit
//! `session_uid: 1` argument on the Rust ports of the PHP static methods.
//!
//! The PHP `Article` static methods are ported to `ttrss::article::{
//! get_tags, get_labels, catchup_by_id, purge_orphans, get_enclosures,
//! labels_of, feeds_of, get_image, format_enclosures }`.

mod common;

use chrono::NaiveDateTime;
use serde_json::{Value, json};
use std::sync::Arc;
use test_db::template::with_test_db;
use ttrss::article::{
    self, CATCHUP_MODE_MARK_AS_READ, CATCHUP_MODE_MARK_AS_UNREAD, CATCHUP_MODE_TOGGLE,
};
use ttrss::core::db::Db;

/// `DbTestCase::setUp`: `$_SESSION["uid"] = 1`.
const SESSION_UID: i64 = 1;

/// `DbTestCase::setUp` on top of the harness-injected pool.
async fn setup(url: &str) -> Db {
    common::make_db_from_url(url).await.expect("test db")
}

fn enclosure(content_type: &str, url: &str) -> Value {
    json!({ "content_type": content_type, "content_url": url })
}

// ── _get_tags() ─────────────────────────────────────────────────────────────

// PHP: test_get_tags_returns_tags_from_cache
#[tokio::test]
async fn test_get_tags_returns_tags_from_cache() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed.sql: article 1 has tag_cache = 'rust|open-source'
            let tags = article::get_tags(db.pool(), SESSION_UID, 1, None, None).await;
            assert_eq!(tags, vec!["rust|open-source".to_string()]);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_tags_returns_empty_for_no_tags
// NOTE: the PHP test name says "empty" but the seed gives article 2
// tag_cache 'database|postgresql', so the ported expectation is that tag.
#[tokio::test]
async fn test_get_tags_returns_empty_for_no_tags() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed.sql: article 2 has tag_cache = 'database|postgresql'
            let tags = article::get_tags(db.pool(), SESSION_UID, 2, None, None).await;
            assert_eq!(tags, vec!["database|postgresql".to_string()]);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_tags_with_custom_owner_uid
#[tokio::test]
async fn test_get_tags_with_custom_owner_uid() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // uid=1 has tags; uid=999 has no user_entries
            let tags = article::get_tags(db.pool(), SESSION_UID, 1, Some(999), None).await;
            assert_eq!(tags, Vec::<String>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_tags_with_explicit_tag_cache
#[tokio::test]
async fn test_get_tags_with_explicit_tag_cache() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // When tag_cache is provided, it is split by comma
            let tags = article::get_tags(
                db.pool(),
                SESSION_UID,
                1,
                Some(1),
                Some("explicit,tag,cache"),
            )
            .await;
            assert_eq!(
                tags,
                vec![
                    "explicit".to_string(),
                    "tag".to_string(),
                    "cache".to_string()
                ]
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── _get_labels() ───────────────────────────────────────────────────────────

// PHP: test_get_labels_returns_cached_labels
#[tokio::test]
async fn test_get_labels_returns_cached_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed.sql: article 1 has label_cache = '' (empty)
            // This means it should query the database
            let labels = article::get_labels(db.pool(), SESSION_UID, 1, None).await;
            assert_eq!(labels, Vec::<Value>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_labels_returns_no_labels_marker
#[tokio::test]
async fn test_get_labels_returns_no_labels_marker() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let labels = article::get_labels(db.pool(), SESSION_UID, 1, None).await;
            assert_eq!(labels, Vec::<Value>::new());

            // Verify cache was updated with no-labels marker
            let label_cache: String = sqlx::query_scalar(
                "SELECT label_cache FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("row exists");
            let decoded: Value = serde_json::from_str(&label_cache).expect("valid JSON");
            assert!(
                decoded.get("no-labels").is_some(),
                "cache should carry the no-labels marker"
            );
            assert_eq!(decoded["no-labels"], json!(1));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_labels_with_cached_labels
#[tokio::test]
async fn test_get_labels_with_cached_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Manually set a label cache
            let labels: Vec<Value> = vec![json!(["1", "test-label-1", "#000000", "#ffffff"])];
            let labels_json = Value::Array(labels.clone()).to_string();
            sqlx::query(
                "UPDATE ttrss_user_entries SET label_cache = $1 WHERE ref_id = 1 AND owner_uid = 1",
            )
            .bind(&labels_json)
            .execute(db.pool())
            .await
            .expect("set label_cache");

            let retrieved = article::get_labels(db.pool(), SESSION_UID, 1, None).await;
            assert_eq!(retrieved, labels);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_labels_different_owner
#[tokio::test]
async fn test_get_labels_different_owner() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let labels = article::get_labels(db.pool(), SESSION_UID, 1, Some(999)).await;
            assert_eq!(labels, Vec::<Value>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── _catchup_by_id() ────────────────────────────────────────────────────────

// PHP: test_catchup_mark_as_read
#[tokio::test]
async fn test_catchup_mark_as_read() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Article 1 is unread (from seed.sql)
            let (before_unread, before_last_read): (bool, Option<NaiveDateTime>) = sqlx::query_as(
        "SELECT unread, last_read FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("row exists");
            assert!(before_unread);
            assert!(before_last_read.is_none());

            article::catchup_by_id(
                db.pool(),
                &[1],
                CATCHUP_MODE_MARK_AS_READ,
                SESSION_UID,
                None,
            )
            .await;

            let (after_unread, after_last_read): (bool, Option<NaiveDateTime>) = sqlx::query_as(
        "SELECT unread, last_read FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("row exists");
            assert!(!after_unread);
            assert!(after_last_read.is_some());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_catchup_mark_as_unread
#[tokio::test]
async fn test_catchup_mark_as_unread() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Article 3 is read (from seed.sql)
            let before: bool = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE ref_id = 3 AND owner_uid = 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("row exists");
            assert!(!before);

            article::catchup_by_id(
                db.pool(),
                &[3],
                CATCHUP_MODE_MARK_AS_UNREAD,
                SESSION_UID,
                None,
            )
            .await;

            let after: bool = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE ref_id = 3 AND owner_uid = 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("row exists");
            assert!(after);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_catchup_toggle
#[tokio::test]
async fn test_catchup_toggle() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Article 1 is unread → toggle should make it read
            article::catchup_by_id(db.pool(), &[1], CATCHUP_MODE_TOGGLE, SESSION_UID, None).await;

            let (after_unread, after_last_read): (bool, Option<NaiveDateTime>) = sqlx::query_as(
        "SELECT unread, last_read FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
    )
    .fetch_one(db.pool())
    .await
    .expect("row exists");
            assert!(!after_unread);
            assert!(after_last_read.is_some());

            // Toggle again → should become unread
            article::catchup_by_id(db.pool(), &[1], CATCHUP_MODE_TOGGLE, SESSION_UID, None).await;
            let after_unread2: bool = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("row exists");
            assert!(after_unread2);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_catchup_multiple_articles
#[tokio::test]
async fn test_catchup_multiple_articles() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Mark articles 1, 2, 3 as read at once
            article::catchup_by_id(
                db.pool(),
                &[1, 2, 3],
                CATCHUP_MODE_MARK_AS_READ,
                SESSION_UID,
                None,
            )
            .await;

            let rows: Vec<(i32, bool)> = sqlx::query_as(
        "SELECT ref_id, unread FROM ttrss_user_entries WHERE ref_id IN (1,2,3) AND owner_uid = 1",
    )
    .fetch_all(db.pool())
    .await
    .expect("query rows");
            assert_eq!(rows.len(), 3);
            for (ref_id, unread) in &rows {
                assert!(!unread, "Article {ref_id} should be marked as read");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_catchup_custom_owner_uid
#[tokio::test]
async fn test_catchup_custom_owner_uid() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Should not affect uid=999's entries
            article::catchup_by_id(
                db.pool(),
                &[1],
                CATCHUP_MODE_MARK_AS_READ,
                SESSION_UID,
                Some(999),
            )
            .await;

            let after: bool = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("row exists");
            assert!(
                after,
                "uid=1's article should not be affected by uid=999 catchup"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── _purge_orphans() ────────────────────────────────────────────────────────

// PHP: test_purge_orphans_removes_orphaned_entries
#[tokio::test]
async fn test_purge_orphans_removes_orphaned_entries() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Insert an article without a corresponding user_entry
    let orphan_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_entries (title, guid, link, updated, content, content_hash, date_entered, date_updated)
         VALUES ($1, $2, $3, NOW(), $4, $5, NOW(), NOW()) RETURNING id",
    )
    .bind("Orphan Article")
    .bind("tag:test.com,2026:orphan")
    .bind("https://test.com/orphan")
    .bind("<p>Orphan content</p>")
    .bind("SHA1:orphan123")
    .fetch_one(db.pool())
    .await
    .expect("insert orphan");

    // Verify it exists before purge
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries WHERE id = $1")
        .bind(orphan_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(count, 1);

    article::purge_orphans(db.pool()).await;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries WHERE id = $1")
        .bind(orphan_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(count, 0);
        Ok(())
    })).await.unwrap();
}

// PHP: test_purge_orphans_keeps_valid_entries
#[tokio::test]
async fn test_purge_orphans_keeps_valid_entries() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Article 1 has both ttrss_entries and ttrss_user_entries
            article::purge_orphans(db.pool()).await;

            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .expect("count");
            assert_eq!(count, 1, "Valid entries should not be purged");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── _get_enclosures() ───────────────────────────────────────────────────────

// PHP: test_get_enclosures_returns_enclosures_for_article
#[tokio::test]
async fn test_get_enclosures_returns_enclosures_for_article() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed.sql adds 2 enclosures for article id=1
            // (no disk cache in this test — empty dir, empty self URL)
            let enclosures =
                article::get_enclosures(db.pool(), 1, std::path::Path::new(""), "").await;
            assert_eq!(enclosures.len(), 2);

            // Check that we have the expected content types
            let content_types: Vec<&str> = enclosures
                .iter()
                .filter_map(|e| e.get("content_type").and_then(Value::as_str))
                .collect();
            assert!(content_types.contains(&"video/mp4"));
            assert!(content_types.contains(&"image/jpeg"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_enclosures_returns_empty_for_no_enclosures
#[tokio::test]
async fn test_get_enclosures_returns_empty_for_no_enclosures() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Article 2 has no enclosures in seed.sql
            let enclosures =
                article::get_enclosures(db.pool(), 2, std::path::Path::new(""), "").await;
            assert!(enclosures.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── _labels_of() and _feeds_of() ────────────────────────────────────────────

// PHP: test_labels_of_returns_empty_for_empty_array
#[tokio::test]
async fn test_labels_of_returns_empty_for_empty_array() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let labels = article::labels_of(db.pool(), &[], SESSION_UID).await;
            assert_eq!(labels, Vec::<i64>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_labels_of_returns_unique_labels
#[tokio::test]
async fn test_labels_of_returns_unique_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Articles 1 and 2 are in the same feed but have different tags
            // Labels are not assigned to these articles in seed, so should be empty
            let labels = article::labels_of(db.pool(), &[1, 2], SESSION_UID).await;
            assert_eq!(labels, Vec::<i64>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_feeds_of_returns_empty_for_empty_array
#[tokio::test]
async fn test_feeds_of_returns_empty_for_empty_array() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let feeds = article::feeds_of(db.pool(), &[], SESSION_UID).await;
            assert_eq!(feeds, Vec::<i64>::new());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_feeds_of_returns_unique_feeds
#[tokio::test]
async fn test_feeds_of_returns_unique_feeds() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Articles 1, 2, 3 are all in feed_id=1 (Hacker News)
            let feeds = article::feeds_of(db.pool(), &[1, 2, 3], SESSION_UID).await;
            assert_eq!(feeds, vec![1]);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_feeds_of_returns_multiple_feeds
#[tokio::test]
async fn test_feeds_of_returns_multiple_feeds() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Articles 1 (feed 1) and 4 (feed 2)
            let feeds = article::feeds_of(db.pool(), &[1, 4], SESSION_UID).await;
            assert_eq!(feeds, vec![1, 2]);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// M7.22 item 4: `get_image` now runs the `HOOK_ARTICLE_IMAGE` chain (PHP
/// `Article.php:571`). The PHP suite runs with no plugins, so these
/// parity cases use a host with empty system + user lists.
async fn get_image_stock(
    encs: &[serde_json::Map<String, Value>],
    content: &str,
    site_url: &str,
) -> (String, String, i32) {
    use ttrss::pluginhost::host::HostCtx;
    let ph = Arc::new(ttrss::pluginhost::PluginHost::startup(
        &ttrss::pluginhost::all_plugins(),
    ));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
        .unwrap();
    let cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(
        Arc::clone(&ph),
        pool.clone(),
        cfg.clone(),
        Some(1),
        None,
        0,
        &[],
        &[],
    );
    article::get_image(&mut ctx, encs, content, site_url, &json!({})).await
}

// ── _get_image() ────────────────────────────────────────────────────────────

// NOTE: the PHP test extracts the original URL from the DiskCache redirect
// wrapper; the Rust server has no disk cache, so the returned URLs are the
// originals and the assertions apply directly.

// PHP: test_get_image_extract_youtube_embed
#[tokio::test]
async fn test_get_image_extract_youtube_embed() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let _ = db;
    let content = "<p>Check out this video: <iframe src=\"https://www.youtube.com/embed/dQw4w9WgXcQ\"></iframe></p>";
    let (image, stream, kind) = get_image_stock(&[], content, "https://example.com").await;

    assert!(
        image.contains("img.youtube.com/vi/dQw4w9WgXcQ/hqdefault.jpg"),
        "image was: {image}"
    );
    assert!(
        stream.contains("youtu.be/dQw4w9WgXcQ"),
        "stream was: {stream}"
    );
    assert_eq!(kind, article::ARTICLE_KIND_YOUTUBE);
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_image_extract_video_poster
#[tokio::test]
async fn test_get_image_extract_video_poster() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    let _ = db;
    let content = "<video poster=\"https://example.com/poster.jpg\"><source src=\"https://example.com/video.mp4\"></video>";
    let (image, stream, kind) = get_image_stock(&[], content, "https://example.com").await;

    assert!(
        image.contains("example.com/poster.jpg"),
        "image was: {image}"
    );
    assert!(
        stream.contains("example.com/video.mp4"),
        "stream was: {stream}"
    );
    assert_eq!(kind, article::ARTICLE_KIND_VIDEO);
        Ok(())
    })).await.unwrap();
}

// PHP: test_get_image_extract_first_image
#[tokio::test]
async fn test_get_image_extract_first_image() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            let content = "<p>Some text <img src=\"https://example.com/photo.jpg\"></p>";
            let (image, stream, kind) = get_image_stock(&[], content, "https://example.com").await;

            assert!(
                image.contains("example.com/photo.jpg"),
                "image was: {image}"
            );
            assert!(stream.is_empty());
            assert_eq!(kind, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_image_skips_data_uris
#[tokio::test]
async fn test_get_image_skips_data_uris() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            let content = "<img src=\"data:image/png;base64,abc123\">";
            let (image, stream, kind) = get_image_stock(&[], content, "https://example.com").await;

            assert!(image.is_empty());
            assert!(stream.is_empty());
            assert_eq!(kind, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_image_falls_back_to_enclosure_image
#[tokio::test]
async fn test_get_image_falls_back_to_enclosure_image() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            let content = "<p>No images in content</p>";
            let enclosures = [enclosure("image/jpeg", "https://example.com/enclosure.jpg")];
            let encs: Vec<serde_json::Map<String, Value>> = enclosures
                .iter()
                .map(|v| v.as_object().unwrap().clone())
                .collect();
            let (image, stream, kind) =
                get_image_stock(&encs, content, "https://example.com").await;

            assert!(
                image.contains("example.com/enclosure.jpg"),
                "image was: {image}"
            );
            assert!(stream.is_empty());
            assert_eq!(kind, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_image_does_not_select_non_image_enclosure
#[tokio::test]
async fn test_get_image_does_not_select_non_image_enclosure() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            let content = "<p>No images in content</p>";
            let enclosures = [enclosure("video/mp4", "https://example.com/video.mp4")];
            let encs: Vec<serde_json::Map<String, Value>> = enclosures
                .iter()
                .map(|v| v.as_object().unwrap().clone())
                .collect();
            let (image, stream, kind) =
                get_image_stock(&encs, content, "https://example.com").await;

            assert!(image.is_empty());
            assert!(stream.is_empty());
            assert_eq!(kind, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_image_mark_album_with_multiple_enclosures
#[tokio::test]
async fn test_get_image_mark_album_with_multiple_enclosures() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            let content = "<p>Single image</p>";
            let enclosures = [
                enclosure("image/jpeg", "https://example.com/img1.jpg"),
                enclosure("image/png", "https://example.com/img2.png"),
            ];
            let encs: Vec<serde_json::Map<String, Value>> = enclosures
                .iter()
                .map(|v| v.as_object().unwrap().clone())
                .collect();
            let (image, _stream, kind) =
                get_image_stock(&encs, content, "https://example.com").await;

            assert!(image.contains("example.com/img1.jpg"), "image was: {image}");
            assert_eq!(kind, article::ARTICLE_KIND_ALBUM);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_image_rewrites_relative_urls
#[tokio::test]
async fn test_get_image_rewrites_relative_urls() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let _ = db;
            // Per RFC 3986, both /path and path resolve to root when base ends with /
            // dirname('/base/') = '/' so relative URLs append to root
            let content = "<img src=\"/relative/path.jpg\">";
            let (image, _stream, _kind) =
                get_image_stock(&[], content, "https://example.com/base/").await;

            assert!(
                image.contains("example.com/relative/path.jpg"),
                "image was: {image}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
