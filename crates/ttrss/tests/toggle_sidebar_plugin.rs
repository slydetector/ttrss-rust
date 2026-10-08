//! Test: M7.16 — the `toggle_sidebar` plugin (main toolbar button), plus the
//! M7.22 handler wiring: `HOOK_MAIN_TOOLBAR_BUTTON` output must reach the
//! shell's `{{ main_toolbar_buttons }}` slot (PHP `index.php:195`).

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use std::sync::Arc;
use test_db::template::with_test_db;
use tower::ServiceExt;

use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::Plugin;
use ttrss::plugins::toggle_sidebar::ToggleSidebar;

fn test_config(plugins: &str) -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = plugins.into();
    cfg
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
        &["toggle_sidebar"],
    )
}

// `hook_main_toolbar_button` renders the toggle button.
#[tokio::test]
async fn test_toolbar_button() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "toggle_sidebar").await;
            let mut host = ctx(&state);
            let plugin = ToggleSidebar;

            plugin.hook_main_toolbar_button(&mut host).await;

            let output = &host.output;
            assert!(output.contains("dijit.form.Button"));
            assert!(output.contains("Plugins.Toggle_Sidebar.toggle"));
            assert!(output.contains("chevron_left"));
            assert!(output.contains("toggle-sidebar-label"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `get_js` returns the toggle JS.
#[tokio::test]
async fn test_get_js() {
    with_test_db(|url| {
        Box::pin(async move {
            setup(&url, "toggle_sidebar").await;
            let plugin = ToggleSidebar;

            let js = plugin.get_js();
            assert!(js.contains("Plugins.Toggle_Sidebar"));
            assert!(js.contains("Feeds.toggle"));
            assert!(js.contains("chevron_left"));
            assert!(js.contains("chevron_right"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── M7.22 — the shell handler wiring (router-level, real session) ──────────

/// Persist a logged-in session (uid 1) and GET `/index.php` with its cookie,
/// mirroring the browser's authenticated shell request.
async fn shell_page(state: &AppState) -> String {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    ttrss::session::save(&state.db, None, "test-sid", &data)
        .await
        .expect("session save");
    let req = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss/index.php")
        .header("cookie", "ttrss_sid=test-sid")
        .body(Body::empty())
        .expect("request");
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "shell must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

/// Active plugin (system level) → the button row appears between the
/// order-0 and order-5 toolbar markers.
#[tokio::test]
async fn test_shell_carries_toolbar_button_when_plugin_active() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "toggle_sidebar").await;
            let html = shell_page(&state).await;
            let marker_before = "<!-- order 0, default -->";
            let marker_after = "<!-- order 5: alert icons -->";
            let b = html
                .find(marker_before)
                .expect("order-0 marker present (shell, not login page)");
            let a = html.find(marker_after).expect("order-5 marker present");
            let row = &html[b + marker_before.len()..a];
            assert!(
                row.contains("dijit.form.Button"),
                "button row expected between the markers: {row}"
            );
            assert!(row.contains("Plugins.Toggle_Sidebar.toggle"));
            assert!(row.contains("toggle-sidebar-label"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// No active plugin → the slot is empty (nothing but whitespace between the
/// markers) — the PHP fallback for the stock set minus `toggle_sidebar`.
#[tokio::test]
async fn test_shell_toolbar_slot_empty_when_plugin_inactive() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "").await;
            let html = shell_page(&state).await;
            let marker_before = "<!-- order 0, default -->";
            let marker_after = "<!-- order 5: alert icons -->";
            let b = html
                .find(marker_before)
                .expect("order-0 marker present (shell, not login page)");
            let a = html.find(marker_after).expect("order-5 marker present");
            let row = &html[b + marker_before.len()..a];
            assert!(row.trim().is_empty(), "no toolbar button expected: {row}");
            assert!(!html.contains("toggle-sidebar-label"));
            Ok(())
        })
    })
    .await
    .unwrap();
}
