//! Test: M7.15 — the `af_psql_trgm` plugin (the trigram-similarity filter).

mod common;

use std::sync::Arc;
use test_db::template::with_test_db;

use serde_json::json;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::Plugin;
use ttrss::plugins::af_psql_trgm::AfPsqlTrgm;

fn test_config(plugins: &str) -> Config {
    let mut c = common::test_config();
    c.plugins = plugins.into();
    c
}

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

fn ctx(state: &AppState) -> HostCtx {
    HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(1),
        None,
        0,
        &[],
        &["af_psql_trgm"],
    )
}

/// Insert a feed and return its id.
async fn insert_feed(pool: &sqlx::PgPool) -> i32 {
    sqlx::query_scalar(
        "INSERT INTO ttrss_feeds (owner_uid, title, feed_url, site_url) \
         VALUES (1, 'Test Feed', 'http://example.com/rss', 'http://example.com') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Insert an entry and link it to the feed + owner. Returns the entry id.
async fn insert_entry(pool: &sqlx::PgPool, feed_id: i32, guid: &str, title: &str) -> i32 {
    let entry_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_entries (title, guid, link, updated, content, content_hash, date_entered, date_updated) \
         VALUES ($1, $2, 'http://example.com/a', NOW(), 'content', 'hash', NOW(), NOW()) RETURNING id",
    )
    .bind(title)
    .bind(guid)
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, tag_cache, label_cache) \
         VALUES ($1, $2, $3, 1, '', '')",
    )
    .bind(entry_id)
    .bind(format!("uuid-{}", guid))
    .bind(feed_id)
    .execute(pool)
    .await
    .unwrap();

    entry_id
}

// `save` returns the "Data saved" message.
#[tokio::test]
async fn test_save() {
    with_test_db(|url| {
        Box::pin(async move {
            setup(&url, "af_psql_trgm").await;

            let msg = AfPsqlTrgm::save(0.85, 32, true);
            assert_eq!(msg, "Data saved (0.85, on)");

            let msg = AfPsqlTrgm::save(-0.1, -5, false);
            assert_eq!(msg, "Data saved (0.00, off)");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `hook_article_button` renders the `bookmark_outline` icon.
#[tokio::test]
async fn test_article_button() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let mut host = ctx(&state);
            let plugin = AfPsqlTrgm;

            let line = json!({ "id": 42 });
            let html = plugin.hook_article_button(&mut host, &line).await;
            assert!(html.contains("bookmark_outline"));
            assert!(html.contains("showRelated(42)"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `hook_prefs_tab("prefFeeds")` renders the AccordionPane with the form fields.
#[tokio::test]
async fn test_prefs_tab() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let mut host = ctx(&state);
            let plugin = AfPsqlTrgm;

            plugin.hook_prefs_tab(&mut host, "prefFeeds").await;

            let output = &host.output;
            assert!(output.contains("dijit.layout.AccordionPane"));
            assert!(output.contains("af_psql_trgm"));
            assert!(output.contains("psql_trgm_similarity"));
            assert!(output.contains("min_title_length"));
            assert!(output.contains("enable_globally"));
            // The full reference form (M7.22 decision-3 fidelity): the
            // pluginhandler save tags, the dojo onSubmit script, the notice, the
            // tooltip, the submit button.
            assert!(
                output.contains("PluginHandler"),
                "missing the pluginhandler op tag"
            );
            assert!(output.contains("name=\"method\""), "missing the method tag");
            assert!(
                output.contains("value=\"save\""),
                "missing the method=save value"
            );
            assert!(
                output.contains("dojo/method"),
                "missing the onSubmit script"
            );
            assert!(output.contains("Enable for specific feeds in the feed editor."));
            assert!(
                output.contains("dijit.Tooltip"),
                "missing the similarity tooltip"
            );
            assert!(
                output.contains("value=\"0.75\""),
                "default similarity value"
            );
            assert!(output.contains("Save"));

            // A different tab produces no output.
            host.output.clear();
            plugin.hook_prefs_tab(&mut host, "otherTab").await;
            assert!(host.output.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The filter returns false when the feed is not enabled.
#[tokio::test]
async fn test_filter_not_enabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let pool = state.db.pool();

            let result = AfPsqlTrgm::filter(
                pool,
                "This is a test title that is long enough to pass the minimum length check",
                "guid123",
                1,
                false, // enable_globally = false
                &[],   // enabled_feeds = empty
                1,     // feed_id
                0.75,
                32,
            )
            .await;
            assert!(!result);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The filter returns false when the title is too short.
#[tokio::test]
async fn test_filter_too_short() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let pool = state.db.pool();

            let result = AfPsqlTrgm::filter(
                pool,
                "short",
                "guid123",
                1,
                true,
                &[],
                1,
                0.75,
                32, // "short" is 5 chars < 32
            )
            .await;
            assert!(!result);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// A similar article (≥ threshold) is caught by the filter.
#[tokio::test]
async fn test_filter_similar() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let pool = state.db.pool();

            let feed_id = insert_feed(pool).await;

            // Insert two similar articles (recently entered).
            insert_entry(
                pool,
                feed_id,
                "guid-a",
                "Breaking: Major software update released for all platforms",
            )
            .await;
            insert_entry(
                pool,
                feed_id,
                "guid-b",
                "Breaking: Major software update released for all platforms today",
            )
            .await;

            // The second article should be caught (similar to the first).
            let result = AfPsqlTrgm::filter(
                pool,
                "Breaking: Major software update released for all platforms today",
                "guid-b",
                1,
                true, // enable_globally
                &[],
                feed_id,
                0.75,
                32,
            )
            .await;
            assert!(result, "expected the similar article to be caught");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `showrelated` returns the related-articles HTML.
#[tokio::test]
async fn test_showrelated() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let pool = state.db.pool();

            let feed_id = insert_feed(pool).await;
            let a1 = insert_entry(
                pool,
                feed_id,
                "guid-c",
                "The quick brown fox jumps over the lazy dog",
            )
            .await;
            insert_entry(
                pool,
                feed_id,
                "guid-d",
                "The quick brown fox leaps over the lazy dog",
            )
            .await;

            let html = AfPsqlTrgm::showrelated(pool, 1, a1).await.unwrap();
            assert!(!html.is_empty());
            assert!(html.contains("panel-scrollable"));
            assert!(html.contains("Close this window"));
            assert!(html.contains("leaps over the lazy dog"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `showrelated` for a missing article returns an empty string.
#[tokio::test]
async fn test_showrelated_missing() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let pool = state.db.pool();

            let html = AfPsqlTrgm::showrelated(pool, 1, 999999).await.unwrap();
            assert!(html.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── M7.22 item 9 — the feed-edit payload composition seam ──────────────────

/// A session carrying `uid = 1` (owner of the fixture feeds).
fn sess(uid: i32) -> ttrss::session::Sess {
    use ttrss::core::phpser::Value as PValue;
    use ttrss::core::session::Session;
    let mut data = Session::new();
    data.set("uid", PValue::Int(uid as i64));
    let s = ttrss::session::Sess::new("test-sid".to_string());
    s.replace("test-sid".to_string(), data);
    s
}

/// Active plugin (system level) → the feed-edit payload's `plugin_data`
/// carries the per-feed similarity checkbox. Regression: the
/// `HOOK_PREFS_EDIT_FEED` chain was once built with `&[], &[]` (zero active
/// plugins), so this was always empty.
#[tokio::test]
async fn test_editfeed_payload_carries_trgm_checkbox_when_active() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "af_psql_trgm").await;
            let extra: serde_json::Map<String, serde_json::Value> =
                serde_json::json!({ "id": 1 }).as_object().unwrap().clone();

            let v = ttrss::feeds::editfeed(&state, &sess(1), &extra).await;
            let plugin_data = v["plugin_data"].as_str().unwrap_or("");
            assert!(
                plugin_data.contains("trgm_similarity_enabled"),
                "plugin_data must carry the per-feed checkbox: {plugin_data}"
            );
            assert!(plugin_data.contains("Similarity (af_psql_trgm)"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// No active plugin → `plugin_data` is empty (the stock-set-minus-trgm
/// baseline).
#[tokio::test]
async fn test_editfeed_payload_plugin_data_empty_when_inactive() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "").await;
            let extra: serde_json::Map<String, serde_json::Value> =
                serde_json::json!({ "id": 1 }).as_object().unwrap().clone();

            let v = ttrss::feeds::editfeed(&state, &sess(1), &extra).await;
            assert_eq!(v["plugin_data"].as_str().unwrap_or(""), "");
            Ok(())
        })
    })
    .await
    .unwrap();
}
