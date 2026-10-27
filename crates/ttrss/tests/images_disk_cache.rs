//! The per-feed `cache_images` disk-cache feature, end to end:
//!
//! - **write side** — a feed update downloads the article's media
//!   (`RSSUtils::cache_media` / `cache_enclosures`) to
//!   `<cache_dir>/images/<sha1($abs_url)>` (gated on `ttrss_feeds.cache_images`);
//! - **read side** — the CDM `view` list, the API `getHeadlines`/`getArticle`
//!   rows, the `share` public route, and the `Article::_get_image`/
//!   `_get_enclosures` URL fields rewrite a cached URL to the local
//!   `op=cached&file=images/<sha1>` serve path
//!   (`DiskCache::rewrite_urls` + the PHP's `get_url` wraps).

use crate::common;

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::headlines::view;
use ttrss::session::Sess;
use ttrss::util::sha1_hex;

/// A valid-enough PNG for the content sniffing (the serve path checks
/// magic bytes; the tests only assert URL rewriting, not the bytes).
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

async fn setup(url: &str) -> (AppState, Sess, PathBuf) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let cache = tempfile::tempdir().expect("per-test cache dir");
    let mut cfg = common::test_config();
    cfg.cache_dir = cache.path().to_path_buf();
    let state = AppState::new(cfg, db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set(
        "pwd_hash",
        PValue::Bytes(crate::common::ADMIN_PWD_HASH.as_bytes().to_vec()),
    );
    data.set("access_level", PValue::Int(10));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess, cache.keep())
}

/// The `op=cached` serve URL for a media URL (the `DiskCache::get_url` form
/// over the app `CACHE_DIR`/`images` instance).
fn cached_url(self_url: &str, media_url: &str) -> String {
    format!(
        "{}/public.php?op=cached&file=images/{}",
        self_url.trim_end_matches('/'),
        sha1_hex(media_url)
    )
}

/// Ensure feed 5 (owner 1) exists (the fixture has none).
async fn ensure_feed_5(pool: &sqlx::PgPool) {
    let _ = sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Test feed', 1, 'https://example.com/feed', 60, 0, 'https://example.com')",
    )
    .execute(pool)
    .await;
}

/// Seed an article for feed 5 (owner 1) with the given raw content.
async fn seed_article(pool: &sqlx::PgPool, id: i32, content: &str) {
    ensure_feed_5(pool).await;
    sqlx::query(
        "INSERT INTO ttrss_entries
             (id, title, guid, link, updated, content, content_hash, date_entered, date_updated,
              tsvector_combined, lang)
         VALUES ($1, 'Article', 'tag:example.com,2026:x', 'https://example.com/x',
                 NOW(), $2, 'SHA1:0000000000000000000000000000000000000000',
                 NOW(), NOW(), to_tsvector('simple', 'article body'), 'en')",
    )
    .bind(id)
    .bind(content)
    .execute(pool)
    .await
    .expect("entry insert");
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
         VALUES ($1, $2, 5, 1, true, false, '', '')",
    )
    .bind(id)
    .bind(format!("uuid-{id}"))
    .execute(pool)
    .await
    .expect("user entry insert");
}

#[tokio::test]
async fn test_cdm_view_rewrites_cached_image() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess, cache_dir) = setup(&url).await;
            let pool = state.db.pool();

            let media = "https://example.com/pic.png";
            seed_article(pool, 901, &format!("<p>text</p><img src=\"{media}\">")).await;
            // A locally cached copy of the media.
            let images = cache_dir.join("images");
            std::fs::create_dir_all(&images).unwrap();
            std::fs::write(images.join(sha1_hex(media)), PNG).unwrap();

            let reply = view(
                &state,
                &sess,
                &Map::from_iter(vec![
                    ("feed".to_string(), json!("5")),
                    ("m".to_string(), json!("")),
                ]),
            )
            .await;
            let content = reply
                .pointer("/headlines/content")
                .and_then(Value::as_array)
                .and_then(|rows| rows.iter().find_map(|r| r.get("content")))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                content.contains(&format!("file=images/{}", sha1_hex(media)))
                    && content.contains("op=cached"),
                "the CDM content must carry the op=cached URL: {content}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_cdm_view_rewrites_cached_enclosure_url() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess, cache_dir) = setup(&url).await;
        let pool = state.db.pool();
        let self_url = sess.self_url();

        let media = "https://example.com/v.mp4";
        seed_article(pool, 902, "<p>text</p>").await;
        sqlx::query(
            "INSERT INTO ttrss_enclosures (content_url, content_type, title, duration, post_id, width, height)
             VALUES ($1, 'video/mp4', '', '00:00:10', 902, 0, 0)",
        )
        .bind(media)
        .execute(pool)
        .await
        .unwrap();
        let images = cache_dir.join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join(sha1_hex(media)), b"ftypisom").unwrap();

        let reply = view(
            &state,
            &sess,
            &Map::from_iter(vec![
                ("feed".to_string(), json!("5")),
                ("m".to_string(), json!("")),
            ]),
        )
        .await;
        let enc_url = reply
            .pointer("/headlines/content")
            .and_then(Value::as_array)
            .and_then(|rows| rows.iter().find_map(|r| r.pointer("/enclosures/entries/0/content_url")))
            .and_then(Value::as_str)
            .unwrap_or("");
        assert_eq!(
            enc_url,
            cached_url(&self_url, media),
            "the enclosure entry URL must be the op=cached one"
        );
        Ok(())
    }))
    .await
    .unwrap();
}

#[tokio::test]
async fn test_cdm_view_untouched_when_no_cached_copy() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess, _cache_dir) = setup(&url).await;
            let pool = state.db.pool();

            // No `images/` files at all: URLs pass through unchanged.
            let media = "https://example.com/pic.png";
            seed_article(pool, 903, &format!("<img src=\"{media}\">")).await;

            let reply = view(
                &state,
                &sess,
                &Map::from_iter(vec![
                    ("feed".to_string(), json!("5")),
                    ("m".to_string(), json!("")),
                ]),
            )
            .await;
            let content = reply
                .pointer("/headlines/content")
                .and_then(Value::as_array)
                .and_then(|rows| rows.iter().find_map(|r| r.get("content")))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                content.contains(media),
                "no rewrite without a cached copy: {content}"
            );
            assert!(
                !content.contains("op=cached"),
                "no rewrite without a cached copy: {content}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_api_getheadlines_rewrites_cached_image() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess, cache_dir) = setup(&url).await;
            let pool = state.db.pool();

            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
             VALUES (1, 'ENABLE_API_ACCESS', NULL, 't')",
            )
            .execute(pool)
            .await
            .expect("set ENABLE_API_ACCESS");

            let media = "https://example.com/api-pic.png";
            seed_article(pool, 904, &format!("<img src=\"{media}\">")).await;
            let images = cache_dir.join("images");
            std::fs::create_dir_all(&images).unwrap();
            std::fs::write(images.join(sha1_hex(media)), PNG).unwrap();

            let body = Map::from_iter(vec![
                ("feed_id".to_string(), json!("5")),
                ("num_items".to_string(), json!(10)),
                ("show_content".to_string(), json!(true)),
            ]);
            let (status, out) =
                ttrss::api::before_and_dispatch(&state, &sess, "getheadlines", &body, None, false)
                    .await;
            assert_eq!(status, 0, "API STATUS_OK: {out}");
            let content = out
                .as_array()
                .and_then(|rows| rows.iter().find_map(|r| r.get("content")))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                content.contains(&format!("file=images/{}", sha1_hex(media)))
                    && content.contains("op=cached"),
                "the API row must carry the op=cached URL: {content}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// write side — the feed update's media download
// ---------------------------------------------------------------------------

/// Seed a feed (owner 1, `cache_images` per `$on`) whose XML carries one
/// article with an `<img>` and a media `<enclosure>`, both on `base`.
async fn seed_feed_for_cache(
    pool: &sqlx::PgPool,
    base: &str,
    on: bool,
) -> (i32, String, String, String) {
    let img = format!("{base}/pic.png");
    let enc = format!("{base}/vid.mp4");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\"><channel><title>Cache feed</title><link>{base}/</link>\
         <item><title>Media article</title><link>{base}/a</link>\
         <guid>{base}/a</guid><description><![CDATA[<p>x</p><img src=\"{img}\">]]></description>\
         <enclosure url=\"{enc}\" type=\"video/mp4\" length=\"10\"/>\
         </item></channel></rss>"
    );
    let feed_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_feeds (owner_uid, title, feed_url, site_url, cache_images)
         VALUES (1, 'Cache feed', $1, $2, $3) RETURNING id",
    )
    .bind(format!("{base}/feed"))
    .bind(base)
    .bind(on)
    .fetch_one(pool)
    .await
    .unwrap();
    (feed_id, img, enc, xml)
}

#[tokio::test]
async fn test_feed_update_caches_media_when_enabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess, cache_dir) = setup(&url).await;
            let pool = state.db.pool();

            // Ordered canned responses: the feed fetch (the XML) comes first, then
            // the media downloads (the last response repeats).
            let server = common::MockServer::new(vec![
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: PNG.to_vec(),
                },
            ]);
            let base = server.url("/").trim_end_matches('/').to_string();
            let (feed_id, img, enc, xml) = seed_feed_for_cache(pool, &base, true).await;
            server.set_response(
                0,
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: xml.into_bytes(),
                },
            );

            let flags = ttrss::updates::FeedUpdateFlags {
                force_refetch: true,
                force_rehash: true,
                ..Default::default()
            };
            let outcome = ttrss::updates::fetch_one_feed(
                pool,
                &state.config,
                state.plugins.clone(),
                feed_id,
                &flags,
                None,
            )
            .await
            .expect("fetch");
            assert!(
                outcome.success,
                "the feed update must succeed: {:?}",
                outcome.error
            );

            let images = cache_dir.join("images");
            assert!(
                images.join(sha1_hex(&img)).is_file(),
                "the article's <img> must be downloaded to the images cache"
            );
            assert!(
                images.join(sha1_hex(&enc)).is_file(),
                "the media enclosure must be downloaded to the images cache"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_feed_update_skips_cache_when_disabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess, cache_dir) = setup(&url).await;
            let pool = state.db.pool();

            let server = common::MockServer::new(vec![
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: PNG.to_vec(),
                },
            ]);
            let base = server.url("/").trim_end_matches('/').to_string();
            let (feed_id, img, _enc, xml) = seed_feed_for_cache(pool, &base, false).await;
            server.set_response(
                0,
                common::Canned {
                    status: 200,
                    reason: "OK",
                    headers: Vec::new(),
                    body: xml.into_bytes(),
                },
            );
            let flags = ttrss::updates::FeedUpdateFlags {
                force_refetch: true,
                force_rehash: true,
                ..Default::default()
            };
            let outcome = ttrss::updates::fetch_one_feed(
                pool,
                &state.config,
                state.plugins.clone(),
                feed_id,
                &flags,
                None,
            )
            .await
            .expect("fetch");
            assert!(
                outcome.success,
                "the feed update must succeed: {:?}",
                outcome.error
            );

            // The article ingested (the feed's content), but nothing was cached.
            let ingested: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_entries e, ttrss_user_entries ue
             WHERE e.id = ue.ref_id AND ue.owner_uid = 1 AND ue.feed_id = $1",
            )
            .bind(feed_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(ingested, 1, "the article was ingested");
            assert!(
                !cache_dir.join("images").join(sha1_hex(&img)).exists(),
                "cache_images off: no media download"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
