//! `Digest::prepare_headlines_digest` (classes/Digest.php) — the digest
//! preview (the `previewdigest` RPC, limit 16) and the daemon's digest
//! (limit 1000) both render per-article labels from
//! `Article::_get_labels(ref_id, user_id)` into the HTML template's
//! `<div><em>${ARTICLE_LABELS}</em></div>` slot. The port once hardcoded an
//! empty label string ("empty for the corpus"); this pins the populated form.
//!
//! The seed's articles sit in 2026-05, outside the digest's
//! `NOW() - INTERVAL '1 day'` window, so the test refreshes `date_updated` /
//! `last_updated` of the two picked articles first.

use crate::common;

use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;

const SESSION_UID: i32 = 1;

async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(SESSION_UID as i64));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

#[tokio::test]
async fn test_digest_shows_article_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = setup(&url).await;
            let p = state.db.pool();

            // Push two articles (feeds 1 and 2) into the digest window.
            // `date_updated` is per-article (ttrss_entries); the digest's
            // unqualified `last_updated` resolves to ttrss_feeds.last_updated.
            sqlx::query("UPDATE ttrss_entries SET date_updated = NOW() WHERE id IN (1, 4)")
                .execute(p)
                .await
                .unwrap();
            sqlx::query("UPDATE ttrss_feeds SET last_updated = NOW() WHERE id IN (1, 2)")
                .execute(p)
                .await
                .unwrap();

            // Article 1 gets both labels, article 4 gets label 2.
            sqlx::query(
                "INSERT INTO ttrss_user_labels2 (label_id, article_id)
                 VALUES (1, 1), (2, 1), (2, 4)",
            )
            .execute(p)
            .await
            .unwrap();

            let res = ttrss::digest::prepare_headlines_digest(
                &state,
                SESSION_UID,
                1,
                1000,
                "http://x/tt-rss/",
            )
            .await;
            let html = res.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let text = res.get(3).and_then(|v| v.as_str()).unwrap_or("");

            // Two articles from two feeds, in (category, feed, score DESC,
            // date DESC) order.
            assert_eq!(
                res.get(1).and_then(|v| v.as_i64()),
                Some(2),
                "digest count = 2, got: {res:?}"
            );
            assert_eq!(
                res.get(2).and_then(|v| v.as_array()).map(|a| a.to_vec()),
                Some(vec![serde_json::json!(1), serde_json::json!(4)]),
                "affected ids, got: {res:?}"
            );

            // The HTML template renders the comma-joined captions in the
            // labels slot (Sanitizer: PHP `implode(", ", captions)`).
            assert!(
                html.contains("<div><em>test-label-1, test-label-2</em></div>"),
                "article 1's labels in the HTML digest, got: {html}"
            );
            assert!(
                html.contains("<div><em>test-label-2</em></div>"),
                "article 4's label in the HTML digest, got: {html}"
            );

            // The plain-text template has no labels/excerpt slots
            // (digest_template.txt emits title, updated, link only).
            assert!(
                !text.contains("test-label"),
                "text digest must not carry labels, got: {text}"
            );
            assert!(text.contains("Show HN: I built a distributed task queue in Rust"));
            assert!(text.contains("To unsubscribe, visit your configuration options"));

            // ENABLE_FEED_CATS defaults on: the feed heading is
            // "category / feed".
            assert!(
                html.contains("<h2>Technology / Hacker News</h2>"),
                "category-prefixed feed heading, got: {html}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
