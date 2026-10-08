//! M7.22 item 7 — the `HOOK_PREFS_TAB` / `HOOK_PREFS_TAB_SECTION` call sites
//! across the six `Pref_*::index` tabs, verified through the real
//! `rpc::handler` (the SPA's lazy tab loads). The bar is structure + visible
//! text (the SPA injects the tab HTML via innerHTML; AGENTS.md).
//!
//! Stock implementers and their gates:
//! - nsfw → `HOOK_PREFS_TAB "prefPrefs"` (the Preferences tab pane)
//! - share → `HOOK_PREFS_TAB_SECTION "prefFeedsPublishedGenerated"` (the
//!   Feeds tab's Sharing pane)
//! - bookmarklets / af_psql_trgm / af_comics → `HOOK_PREFS_TAB "prefFeeds"`
//!   (the Feeds tab's plugin panes; the tab wraps them in the "Plugins"
//!   ContentPane only when the chain produced output — PHP's `ob_start`
//!   trim special case)

mod common;

use std::sync::Arc;
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::HostCtx;
use ttrss::session::Sess;

use serde_json::Value;

fn test_config(plugins: &str) -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = plugins.to_string();
    cfg.self_url = "http://127.0.0.1:8280/tt-rss/".to_string();
    cfg
}

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

/// A full session for `uid = 1` (seed admin hash + CSRF token).
fn sess() -> Sess {
    let mut d = Session::new();
    d.set("uid", PValue::Int(1));
    d.set("access_level", PValue::Int(100));
    d.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    d.set("name", PValue::Bytes(b"admin".to_vec()));
    d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let s = Sess::new("prefs-tabs".into());
    s.replace("prefs-tabs".into(), d);
    s
}

/// `backend.php` POST → the response body as a string (HTML or JSON).
async fn call(state: &AppState, body: &str) -> String {
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/backend.php")
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let res = ttrss::rpc::handler(
        axum::extract::State(state.clone()),
        axum::extract::Extension(sess()),
        req,
    )
    .await;
    let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).to_string()
}

// The Feeds tab: the plugin panes (bookmarklets, af_psql_trgm, af_comics)
// are wrapped in the "Plugins" ContentPane, and share's "Unshare all
// articles" button lands in the Sharing pane (the section site).
#[tokio::test]
async fn test_feeds_tab_plugin_panes() {
    with_test_db(|url| Box::pin(async move {
    let state =
        setup(&url, "bookmarklets,af_psql_trgm,af_comics,share").await;
    let html = call(&state, "op=Pref_Feeds&method=index").await;

    // The wrapping pane (PHP: only when the trimmed tab data is non-empty).
    assert!(
        html.contains("<div dojoType='dijit.layout.ContentPane' title=\"<i class='material-icons'>extension</i> Plugins\">"),
        "missing the Plugins ContentPane"
    );
    // The three tab panes.
    assert!(html.contains("Bookmarklets"), "missing the bookmarklets pane: {html}");
    assert!(
        html.contains("Mark similar articles as read (af_psql_trgm)"),
        "missing the af_psql_trgm pane"
    );
    assert!(
        html.contains("Feeds supported by af_comics"),
        "missing the af_comics pane title: {html}"
    );
    assert!(
        html.contains("The following comics are currently supported:"),
        "missing the af_comics header"
    );
    assert!(
        html.contains("Drop any updated filters into <code>filters.local</code>"),
        "missing the af_comics filters notice"
    );
    // The section site: share's button in the Sharing pane.
    assert!(
        html.contains("Unshare all articles"),
        "missing share's prefFeedsPublishedGenerated button"
    );
        Ok(())
    })).await.unwrap();
}

// The Preferences tab: nsfw's pane (the `HOOK_PREFS_TAB "prefPrefs"` site,
// after the Plugins pane).
#[tokio::test]
async fn test_prefs_tab_nsfw_pane() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "nsfw").await;
            {
                let mut c = HostCtx::for_user(
                    Arc::clone(&state.plugins),
                    state.db.pool().clone(),
                    state.config.clone(),
                    Some(1),
                    None,
                    0,
                    &["nsfw"],
                    &[],
                );
                let host = c.host_arc();

                let plugin = host.get_plugin("nsfw").expect("nsfw active");
                c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
                    .await
                    .unwrap();
            }

            let html = call(&state, "op=Pref_Prefs&method=index").await;
            assert!(
                html.contains("NSFW Plugin"),
                "missing the nsfw pane title: {html}"
            );
            assert!(
                html.contains("Tags to consider NSFW (comma-separated):"),
                "missing the nsfw form header"
            );
            assert!(
                html.contains("name='tags'>nsfw"),
                "the stored tags must prefill the textarea: {html}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// No relevant plugins → no panes anywhere (the pre-plugin baseline): the
// Feeds tab has no "Plugins" ContentPane, the Preferences tab no nsfw pane,
// and the section sites contribute nothing.
#[tokio::test]
async fn test_tabs_empty_when_no_plugins() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "").await;

            let feeds = call(&state, "op=Pref_Feeds&method=index").await;
            assert!(
                !feeds.contains("extension</i> Plugins"),
                "no plugin pane expected: {feeds}"
            );
            assert!(
                !feeds.contains("Unshare all articles"),
                "no share button expected"
            );

            let prefs = call(&state, "op=Pref_Prefs&method=index").await;
            assert!(!prefs.contains("NSFW Plugin"), "no nsfw pane expected");

            // The other tabs still render (the chains are no-ops, not errors).
            let filters = call(&state, "op=Pref_Filters&method=index").await;
            assert!(filters.contains("PrefFilterTree"), "filters tab intact");
            let labels = call(&state, "op=Pref_Labels&method=index").await;
            assert!(labels.contains("PrefLabelTree"), "labels tab intact");
            let users = call(&state, "op=Pref_Users&method=index").await;
            assert!(
                users.contains("users-list"),
                "users tab intact (both section sites)"
            );
            let system = call(&state, "op=Pref_System&method=index").await;
            assert!(system.contains("Event log"), "system tab intact");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `setplugins` (the SPA's "Enable selected" save, `Pref_Prefs::setplugins`):
// POST the checked `plugins[]` values and verify `getPluginsList` reflects the
// flipped `user_enabled` pref — then flip it back off. Pref_Prefs read/write
// methods (beyond `index`) are CSRF-gated, so the token is included.
#[tokio::test]
async fn test_setplugins_persists_enabled_set() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "").await;

            // Enable `bookmarklets` (a bundled plugin). Void method (xhr.post) →
            // an empty body.
            let resp = call(
                &state,
                "op=Pref_Prefs&method=setplugins&csrf_token=tok&plugins%5B%5D=bookmarklets",
            )
            .await;
            assert!(
                resp.is_empty(),
                "setplugins must return an empty body: {resp:?}"
            );

            let v: Value = serde_json::from_str(
                &call(&state, "op=Pref_Prefs&method=getPluginsList&csrf_token=tok").await,
            )
            .unwrap();
            let bm = v["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "bookmarklets")
                .expect("bookmarklets listed");
            assert!(
                bm["user_enabled"].as_bool().unwrap_or(false),
                "bookmarklets should be user-enabled"
            );

            // Disable it (no `plugins[]` posted → empty enabled set).
            call(&state, "op=Pref_Prefs&method=setplugins&csrf_token=tok").await;
            let v2: Value = serde_json::from_str(
                &call(&state, "op=Pref_Prefs&method=getPluginsList&csrf_token=tok").await,
            )
            .unwrap();
            let bm2 = v2["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "bookmarklets")
                .unwrap();
            assert!(
                !bm2["user_enabled"].as_bool().unwrap_or(true),
                "bookmarklets should be disabled"
            );

            Ok(())
        })
    })
    .await
    .unwrap();
}

// The five unported plugin-management methods each route to their own
// `tracing::warn!` arm and return E_UNKNOWN_METHOD (a no-op) — not a silent
// success — so the SPA's "falsely confirm" path is at least logged server-side.
#[tokio::test]
async fn test_unported_plugin_mgmt_methods_return_unknown_method() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "bookmarklets").await;
            for method in [
                "installPlugin",
                "uninstallPlugin",
                "checkForPluginUpdates",
                "updateLocalPlugins",
                "clearPluginData",
            ] {
                let body = format!("op=Pref_Prefs&method={method}&csrf_token=tok");
                let v: Value = serde_json::from_str(&call(&state, &body).await).unwrap();
                assert_eq!(
                    v["error"]["code"],
                    Value::String("E_UNKNOWN_METHOD".into()),
                    "{method} must return E_UNKNOWN_METHOD"
                );
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}
