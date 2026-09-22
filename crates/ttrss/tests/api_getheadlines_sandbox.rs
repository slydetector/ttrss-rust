//! DB tests for the `has_sandbox` capability flag (M5a.3).
//!
//! The tt-rss-android client sends `has_sandbox=true` with `getHeadlines` to
//! declare it can render sandboxed iframes. PHP stores it in the session
//! (`$_SESSION['hasSandbox']`); the sanitizer then keeps `<iframe>` elements
//! in article content (hardened with a `sandbox` attribute) instead of
//! stripping them. `getArticle` re-reads the session flag (no param of its
//! own), so the client's full-article view shows the same embeds.

mod common;

use serde_json::{json, Map, Value};
use ttrss::api;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;


/// Fixtures + a fresh session carrying `uid = 1` + the per-user API gate.
async fn setup(
    url: &str,
) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(state.db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
    (state, sess)
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

const IFRAME_HTML: &str =
    "<p>Watch this:</p><iframe src=\"http://example.com/embed\" width=\"560\" height=\"315\"></iframe>";

/// Feed 5 (in fixture cat 1) with one unread entry whose content carries a
/// non-whitelisted iframe.
async fn add_iframe_entry(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Embed feed', 1, 'https://example.com/feed', 60, 0, 'https://example.com')",
    )
    .execute(pool)
    .await
    .expect("feed insert");
    sqlx::query(
        "INSERT INTO ttrss_entries (id, title, guid, link, updated, content, content_hash, lang, date_entered, date_updated)
         VALUES (50, 'Embed article', 'tag:example.com,2026:emb-001',
                 'https://example.com/article', '2026-05-25 10:00:00', $1, 'SHA1:0000000000000000000000000000000000000000', 'en',
                 '2026-05-25 10:00:00', '2026-05-25 10:00:00')",
    )
    .bind(IFRAME_HTML)
    .execute(pool)
    .await
    .expect("entry insert");
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
         VALUES (50, 'uuid-emb-001', 5, 1, true, false, '', '')",
    )
    .execute(pool)
    .await
    .expect("user entry insert");
}

/// The article-50 `content` field of the `getheadlines` response.
async fn headline_content(state: &AppState, sess: &Sess, pairs: &[(&str, Value)]) -> Option<String> {
    let (status, content) =
        api::before_and_dispatch(state, sess, "getheadlines", &request(pairs), None).await;
    assert_eq!(status, 0);
    content
        .as_array()
        .expect("array response")
        .iter()
        .find(|e| e.get("id").and_then(Value::as_i64) == Some(50))
        .and_then(|e| e.get("content").and_then(Value::as_str))
        .map(str::to_string)
}

/// The article-50 `content` field of a `getarticle` response.
async fn article_content(state: &AppState, sess: &Sess) -> Option<String> {
    let (status, content) =
        api::before_and_dispatch(state, sess, "getarticle", &request(&[("article_id", json!([50]))]), None).await;
    assert_eq!(status, 0);
    content
        .as_array()
        .expect("array response")
        .iter()
        .find(|e| e.get("id").and_then(Value::as_i64) == Some(50))
        .and_then(|e| e.get("content").and_then(Value::as_str))
        .map(str::to_string)
}

#[tokio::test]
async fn test_headlines_strips_iframe_without_flag() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = common::make_state(&url).await;
    add_iframe_entry(state.db.pool()).await;

    let content = headline_content(
        &state,
        &sess,
        &[("feed_id", json!("5")), ("show_content", json!(true))],
    )
    .await
    .expect("article 50 in headlines");
    assert!(
        !content.contains("iframe"),
        "iframe must be stripped without has_sandbox: {content}"
    );
    assert!(content.contains("Watch this:"), "text content preserved: {content}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_headlines_keeps_hardened_iframe_with_flag() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = common::make_state(&url).await;
    add_iframe_entry(state.db.pool()).await;

    let content = headline_content(
        &state,
        &sess,
        &[
            ("feed_id", json!("5")),
            ("show_content", json!(true)),
            ("has_sandbox", json!("true")),
        ],
    )
    .await
    .expect("article 50 in headlines");
    // Non-whitelisted src: kept, responsive-wrapped, tightly sandboxed.
    assert!(content.contains("embed-responsive"), "responsive wrapper: {content}");
    assert!(
        content.contains("sandbox=\"allow-scripts\""),
        "sandbox attribute: {content}"
    );
    assert!(content.contains("http://example.com/embed"), "src preserved: {content}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_getarticle_reads_the_session_flag() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = common::make_state(&url).await;
    add_iframe_entry(state.db.pool()).await;

    // A session that never called getHeadlines: flag unset → stripped.
    let content = article_content(&state, &sess).await.expect("article 50");
    assert!(!content.contains("iframe"), "stripped pre-flag: {content}");

    // The client's getHeadlines(has_sandbox=true) sets the session flag…
    let content = headline_content(
        &state,
        &sess,
        &[
            ("feed_id", json!("5")),
            ("show_content", json!(true)),
            ("has_sandbox", json!("true")),
        ],
    )
    .await
    .expect("article 50 in headlines");
    assert!(content.contains("iframe"), "kept in headlines: {content}");

    // …and the same session's getArticle now keeps the embed too.
    let content = article_content(&state, &sess).await.expect("article 50");
    assert!(
        content.contains("sandbox=\"allow-scripts\""),
        "kept in getArticle via session flag: {content}"
    );
        Ok(())
    })).await.unwrap();
}

/// The `score` field in the headlines JSON is a **string** (not a number).
///
/// The frozen frontend (`tt-rss/js/Headlines.js`) MutationObserver reads
/// `data-score` via `row.getAttribute('data-score')` — always a string — and
/// compares it with `!==` against `hl.score` (the initial value from the JSON
/// response). If the backend sends a number, `0 !== "0"` is `true` under
/// strict comparison, causing a spurious `setScore(0)` RPC on any class
/// mutation (e.g. mark-as-read on scroll removes the `Unread` class).
///
/// Sending score as a string ensures `hl.score` from JSON is a string,
/// matching what `getAttribute` returns, so the comparison is `"0" !== "0"`
/// → `false` → no spurious RPC.
#[tokio::test]
async fn test_headlines_score_is_json_string() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = common::make_state(&url).await;
    add_iframe_entry(state.db.pool()).await;

    let (status, content) = api::before_and_dispatch(
        &state, &sess, "getheadlines",
        &request(&[("feed_id", json!("5"))]),
        None,
    )
    .await;
    assert_eq!(status, 0);

    let lines = content.as_array().expect("array response");
    let article = lines
        .iter()
        .find(|e| e.get("id").and_then(Value::as_i64) == Some(50))
        .expect("article 50 present");

    let score = article.get("score").expect("score field present");
    // Must be a JSON string so the frontend strict comparison works.
    assert!(
        score.is_string(),
        "score must be a JSON string (frontend getAttribute parity), got: {score:?}"
    );
    assert_eq!(score.as_str(), Some("0"), "default score is \"0\"");
        Ok(())
    })).await.unwrap();
}
