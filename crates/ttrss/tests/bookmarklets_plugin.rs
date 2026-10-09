//! Test: M7.14 — the `bookmarklets` plugin (the `prefFeeds` prefs-tab UI
//! exposing the two bookmarklets + the `sharepopup` public method).

mod common;

use std::sync::Arc;
use test_db::template::with_test_db;

use serde_json::json;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::pluginhost::dispatch::public_dispatch;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::MethodOutput;

fn test_config(plugins: &str) -> Config {
    let mut c = common::test_config();
    c.plugins = plugins.into();
    c
}

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

/// A `HostCtx` for user 1 with the `bookmarklets` plugin active + the
/// `self_url` set (needed for the bookmarklet URL construction).
fn ctx(state: &AppState) -> HostCtx {
    HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(1),
        None,
        0,
        &[],
        &["bookmarklets"],
    )
    .with_self_url(state.config.self_url.clone())
}

/// The article count in the Published feed (the `owner_uid`'s published
/// articles).
async fn published_count(pool: &sqlx::PgPool, owner: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_entries WHERE id IN \
         (SELECT ref_id FROM ttrss_user_entries WHERE owner_uid = $1 AND published = true)",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap()
}

// `hook_prefs_tab("prefFeeds")` renders the AccordionPane with the two
// bookmarklet links + the "More info…" button.
#[tokio::test]
async fn test_prefs_tab() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;
            let plugin = state
                .plugins
                .get_plugin("bookmarklets")
                .expect("bookmarklets registered");
            let mut c = ctx(&state);

            // A different tab name prints nothing.
            plugin.hook_prefs_tab(&mut c, "otherTab").await;
            assert_eq!(c.output, "");

            plugin.hook_prefs_tab(&mut c, "prefFeeds").await;
            let html = &c.output;

            // The AccordionPane.
            assert!(
                html.contains("dojoType=\"dijit.layout.AccordionPane\""),
                "{html}"
            );
            assert!(html.contains("Bookmarklets"), "{html}");

            // The subscribe bookmarklet (a `javascript:` URI with the confirm dialog).
            assert!(html.contains("javascript:"), "{html}");
            assert!(html.contains("Subscribe in Tiny Tiny RSS"), "{html}");
            assert!(
                html.contains("encodeURIComponent(window.location.href)"),
                "{html}"
            );

            // The share bookmarklet (a `javascript:` URI that opens a popup).
            assert!(html.contains("Share with Tiny Tiny RSS"), "{html}");
            assert!(html.contains("toolbar=0,resizable=0"), "{html}");

            // The "More info…" button.
            assert!(html.contains("More info..."), "{html}");
            assert!(
                html.contains("tt-rss.org/docs/Share-Anything.html"),
                "{html}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `hook_prefs_tab` on a non-`prefFeeds` tab prints nothing.
#[tokio::test]
async fn test_prefs_tab_other_tab() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;
            let plugin = state
                .plugins
                .get_plugin("bookmarklets")
                .expect("bookmarklets registered");
            let mut c = ctx(&state);

            plugin.hook_prefs_tab(&mut c, "prefGeneral").await;
            assert_eq!(c.output, "");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `sharepopup` (no action, logged in) → the form with the pre-filled
// title + URL.
#[tokio::test]
async fn test_sharepopup_form() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;

            let plugin = state
                .plugins
                .get_plugin("bookmarklets")
                .expect("bookmarklets registered");
            let mut c = ctx(&state);

            let v = plugin
                .invoke_method(
                    &mut c,
                    "sharepopup",
                    &json!({"title": "My Page", "url": "http://example.com/"}),
                )
                .await
                .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(body.contains("Share with Tiny Tiny RSS"), "{body}");
            assert!(body.contains("name='title' value='My Page'"), "{body}");
            assert!(
                body.contains("name='url' value='http://example.com/'"),
                "{body}"
            );
            assert!(body.contains("name='action' value='share'"), "{body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `sharepopup` (action=share, logged in) → creates a published article +
// returns the `window.close()` script.
#[tokio::test]
async fn test_sharepopup_share() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;

            let before = published_count(state.db.pool(), 1).await;

            let plugin = state
                .plugins
                .get_plugin("bookmarklets")
                .expect("bookmarklets registered");
            let mut c = ctx(&state);

            let v = plugin
                .invoke_method(
                    &mut c,
                    "sharepopup",
                    &json!({
                        "action": "share",
                        "title": "Shared Article",
                        "url": "http://example.com/shared",
                        "content": "",
                        "labels": "test"
                    }),
                )
                .await
                .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(body.contains("window.close()"), "{body}");

            let after = published_count(state.db.pool(), 1).await;
            assert!(after > before, "published article count should increase");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `sharepopup` (unauthenticated) → the login form.
#[tokio::test]
async fn test_sharepopup_not_logged_in() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;

            // The public dispatch uses owner 0 (unauthenticated) — but the
            // `sharepopup` method checks `uid == 0` for the login form.
            // Since the public dispatch sets owner_uid to 0, this triggers the
            // login form.
            let v = public_dispatch(
                &state,
                "bookmarklets",
                "sharepopup",
                &json!({}),
                &state.config.self_url_rtrim(),
            )
            .await
            .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(body.contains("Not logged in"), "{body}");
            assert!(body.contains("name=\"login\""), "{body}");
            assert!(body.contains("name=\"password\""), "{body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `subscribe` (no feed_url, logged in) → the form.
#[tokio::test]
async fn test_subscribe_form() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;

            let plugin = state
                .plugins
                .get_plugin("bookmarklets")
                .expect("bookmarklets registered");
            let mut c = ctx(&state);

            let v = plugin
                .invoke_method(&mut c, "subscribe", &json!({"csrf_token": "tok"}))
                .await
                .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(body.contains("Subscribe to feed..."), "{body}");
            assert!(body.contains("name=\"feed_url\""), "{body}");
            assert!(body.contains("name=\"csrf_token\" value=\"tok\""), "{body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `subscribe` (unauthenticated) → the login form.
#[tokio::test]
async fn test_subscribe_not_logged_in() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;

            let v = public_dispatch(
                &state,
                "bookmarklets",
                "subscribe",
                &json!({}),
                &state.config.self_url_rtrim(),
            )
            .await
            .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(body.contains("Not logged in"), "{body}");
            assert!(body.contains("name=\"login\""), "{body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}
