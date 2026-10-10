//! The remaining G1 dispatch sites, pinned by a single stub plugin:
//! the shell hooks (`HOOK_FEED_TREE` / `HOOK_TOOLBAR_BUTTON` /
//! `HOOK_ACTION_ITEM`), `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`,
//! `HOOK_PREFS_TABS`, `HOOK_VALIDATE_SESSION`,
//! `HOOK_HEADLINES_CUSTOM_SORT_MAP` (API `custom_sort_types`),
//! `HOOK_UNSUBSCRIBE_FEED`, `HOOK_GET_FULL_TEXT`, `HOOK_HOTKEY_INFO`,
//! and `HOOK_FORMAT_ARTICLE` (share plugin's article page).
//!
//! No first-party plugin implements any of these, so the dispatch sites are
//! no-ops for the stock set (exactly as in PHP). These tests prove the
//! dispatch itself.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::body::to_bytes;
use axum::extract::{Extension, State};
use axum::http::header;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use test_db::template::with_test_db;
use tower::ServiceExt;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::dispatch::public_dispatch;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::{
    About, HOOK_ACTION_ITEM, HOOK_ARTICLES_MARK_TOGGLED, HOOK_ARTICLES_PUBLISH_TOGGLED,
    HOOK_FEED_TREE, HOOK_FORMAT_ARTICLE, HOOK_GET_FULL_TEXT, HOOK_HEADLINES_CUSTOM_SORT_MAP,
    HOOK_HOTKEY_INFO, HOOK_LOGINFORM_ADDITIONAL_BUTTONS, HOOK_PREFS_TABS, HOOK_TOOLBAR_BUTTON,
    HOOK_UNSUBSCRIBE_FEED, HOOK_VALIDATE_SESSION, Plugin, PluginHost,
};

/// Per-run observable state: set by the stub when the corresponding hook
/// fires, and (for validate) the result the stub returns. One instance per
/// `app()` so parallel tests can't clobber each other's flags.
#[derive(Clone)]
struct DispatchFlags {
    mark_toggled: Arc<AtomicBool>,
    publish_toggled: Arc<AtomicBool>,
    validate_result: Arc<AtomicBool>,
}

/// One system stub for every remaining hook (system so it is active on the
/// login-form / public hosts too).
struct DispatchStub {
    flags: DispatchFlags,
}

#[async_trait]
impl Plugin for DispatchStub {
    fn name(&self) -> &'static str {
        "dispatch_stub"
    }
    fn class_name(&self) -> &'static str {
        "Dispatch_Stub"
    }
    fn about(&self) -> About {
        About {
            system: true,
            ..About::new("stub (test)", "tester")
        }
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_FEED_TREE);
        host.add_hook_default(HOOK_TOOLBAR_BUTTON);
        host.add_hook_default(HOOK_ACTION_ITEM);
        host.add_hook_default(HOOK_LOGINFORM_ADDITIONAL_BUTTONS);
        host.add_hook_default(HOOK_PREFS_TABS);
        host.add_hook_default(HOOK_VALIDATE_SESSION);
        host.add_hook_default(HOOK_HEADLINES_CUSTOM_SORT_MAP);
        host.add_hook_default(HOOK_GET_FULL_TEXT);
        host.add_hook_default(HOOK_FORMAT_ARTICLE);
        host.add_hook_default(HOOK_HOTKEY_INFO);
        host.add_hook_default(HOOK_UNSUBSCRIBE_FEED);
        host.add_hook_default(HOOK_ARTICLES_MARK_TOGGLED);
        host.add_hook_default(HOOK_ARTICLES_PUBLISH_TOGGLED);
        // A plugin-registered API method (G2: the `API::index` catch-all).
        host.add_api_method("apimethod");
    }

    async fn invoke_method(
        &self,
        _host: &mut HostCtx,
        method: &str,
        params: &Value,
    ) -> Result<ttrss::pluginhost::plugin::MethodOutput, ttrss::pluginhost::plugin::PluginError>
    {
        if method == "apimethod" {
            return Ok(ttrss::pluginhost::plugin::MethodOutput::Json(json!({
                "api": "ok",
                "arg": params.get("arg"),
            })));
        }
        Err(ttrss::pluginhost::plugin::PluginError::UnknownMethod(
            method.to_string(),
        ))
    }

    async fn hook_feed_tree(&self, _host: &mut HostCtx) -> String {
        "<div id=\"FT\">feedtree</div>".to_string()
    }
    async fn hook_toolbar_button(&self, _host: &mut HostCtx) -> String {
        "<span id=\"TB\">toolbar</span>".to_string()
    }
    async fn hook_action_item(&self, _host: &mut HostCtx) -> String {
        "<div id=\"AI\">action</div>".to_string()
    }
    async fn hook_loginform_additional_buttons(&self, host: &mut HostCtx) -> () {
        host.output.push_str("<button id=\"LB\">SSO</button>");
    }
    async fn hook_prefs_tabs(&self, host: &mut HostCtx) -> () {
        host.output.push_str("<div id=\"PT\">prefs-tab</div>");
    }
    async fn hook_validate_session(&self, _host: &mut HostCtx) -> bool {
        self.flags.validate_result.load(Ordering::SeqCst)
    }
    async fn hook_headlines_custom_sort_map(&self, _host: &mut HostCtx) -> Value {
        json!({ "custom": "My sort" })
    }
    async fn hook_get_full_text(&self, _host: &mut HostCtx, _url: &str) -> String {
        "<p>FULLTEXT</p>".to_string()
    }
    async fn hook_format_article(&self, _host: &mut HostCtx, html: &str, _row: &Value) -> String {
        format!("FMT::{html}")
    }
    async fn hook_hotkey_info(&self, _host: &mut HostCtx, _hotkeys: &Value) -> Value {
        json!({ "Zed": { "next_feed": "REWRITTEN" } })
    }
    async fn hook_unsubscribe_feed(
        &self,
        _host: &mut HostCtx,
        _feed_id: i64,
        _owner_uid: i64,
    ) -> bool {
        true
    }
    async fn hook_articles_mark_toggled(&self, _host: &mut HostCtx, _ids: &[i64]) -> () {
        self.flags.mark_toggled.store(true, Ordering::SeqCst);
    }
    async fn hook_articles_publish_toggled(&self, _host: &mut HostCtx, _ids: &[i64]) -> () {
        self.flags.publish_toggled.store(true, Ordering::SeqCst);
    }
}

/// An admin session (user 1) saved under `sid`, mirroring the stock
/// `ttrss_users` fixture hash.
async fn save_admin_session(state: &AppState, sid: &str) {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    // The DB admin's access level — login records the DB value, and
    // validate_session rejects a stale one.
    data.set("access_level", PValue::Int(10));
    data.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    ttrss::session::save(&state.db, None, sid, &data)
        .await
        .expect("save session");
}

fn flags() -> DispatchFlags {
    DispatchFlags {
        mark_toggled: Arc::new(AtomicBool::new(false)),
        publish_toggled: Arc::new(AtomicBool::new(false)),
        // Neutral by default (a real host with no hooks returns true).
        validate_result: Arc::new(AtomicBool::new(true)),
    }
}

async fn app(url: &str) -> (AppState, DispatchFlags) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let mut cfg = common::test_config();
    cfg.plugins = "dispatch_stub".to_string();
    let fl = flags();
    let mut state = AppState::new(cfg, db);
    state.plugins = Arc::new({
        let mut host = PluginHost::startup(&ttrss::pluginhost::all_plugins());
        host.register(Box::new(DispatchStub { flags: fl.clone() }));
        host
    });
    (state, fl)
}

async fn get_page(state: &AppState, uri: &str, cookie: Option<&str>) -> String {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    if let Some(c) = cookie {
        req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(axum::http::header::COOKIE, c)
            .body(Body::empty())
            .expect("request");
    }
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "{uri} must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

#[tokio::test]
async fn test_shell_runs_feed_tree_toolbar_action_hooks() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            save_admin_session(&state, "disp-sid").await;
            let html = get_page(&state, "/tt-rss/index.php", Some("ttrss_sid=disp-sid")).await;
            assert!(html.contains("id=\"FT\""), "HOOK_FEED_TREE output missing");
            assert!(
                html.contains("id=\"TB\""),
                "HOOK_TOOLBAR_BUTTON output missing"
            );
            assert!(
                html.contains("id=\"AI\""),
                "HOOK_ACTION_ITEM output missing"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_login_page_runs_loginform_hook() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            let html = get_page(&state, "/tt-rss/index.php", None).await;
            assert!(
                html.contains("id=\"LB\""),
                "HOOK_LOGINFORM_ADDITIONAL_BUTTONS output missing: login page"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_prefs_page_runs_prefs_tabs_hook() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            save_admin_session(&state, "disp-sid").await;
            let html = get_page(&state, "/tt-rss/prefs.php", Some("ttrss_sid=disp-sid")).await;
            assert!(html.contains("id=\"PT\""), "HOOK_PREFS_TABS output missing");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_validate_session_runs_hook() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            // A valid admin session (hash matches the fixture) — the hook returning
            // `false` must invalidate it.
            fl.validate_result.store(false, Ordering::SeqCst);
            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            data.set("access_level", PValue::Int(10));
            data.set(
                "pwd_hash",
                PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
            );
            data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
            data.set("name", PValue::Bytes(b"admin".to_vec()));
            data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
            let ok = ttrss::session::validate_session(&state, false, &mut data).await;
            assert!(!ok, "a hook returning false must invalidate the session");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_getconfig_custom_sort_types() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");

            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            let sess = ttrss::session::Sess::new("sort-types".into());
            sess.replace("sort-types".into(), data);

            let (status, out) = ttrss::api::before_and_dispatch(
                &state,
                &sess,
                "getconfig",
                json!({}).as_object().unwrap(),
                None,
            )
            .await;
            assert_eq!(status, 0, "API STATUS_OK, got {out}");
            assert_eq!(
                out.get("custom_sort_types").and_then(Value::as_object),
                Some(&serde_json::Map::from_iter([(
                    "custom".to_string(),
                    json!("My sort")
                )])),
                "the hook's map must populate custom_sort_types: {out}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_remove_feed_runs_unsubscribe_hook() {
    with_test_db(|url| Box::pin(async move {
    let (state, _fl) = app(&url).await;
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval)
         VALUES (900, 1, 'hook feed', 1, 'https://example.com/feed', 60, 0)",
    )
    .execute(state.db.pool())
    .await
    .expect("feed insert");
    ttrss::feeds::remove_feed(&state, 900, 1).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE id = 900",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1, "a hook returning true must skip the removal");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_hotkey_info_chain() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            let mut ctx = HostCtx::system_plugins(
                Arc::clone(&state.plugins),
                state.db.pool().clone(),
                state.config.clone(),
            );
            let html = ttrss::hotkeys::hotkey_help(&mut ctx).await;
            assert!(
                html.contains("REWRITTEN"),
                "the chain's map must be rendered"
            );
            assert!(
                html.contains("Zed"),
                "the chain's section name must be rendered"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_create_published_article_get_full_text() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            let pctx = ttrss::pluginhost::dispatch::owner_ctx(&state, 1).await;
            let ok = ttrss::article::create_published_article(
                state.db.pool(),
                "Full text article",
                "https://example.com/fulltext",
                "",
                "",
                1,
                pctx,
            )
            .await;
            assert!(ok, "create_published_article succeeded");
            let content: Option<String> = sqlx::query_scalar(
                "SELECT content FROM ttrss_entries WHERE link = 'https://example.com/fulltext'",
            )
            .fetch_optional(state.db.pool())
            .await
            .unwrap();
            assert_eq!(content.as_deref(), Some("<p>FULLTEXT</p>"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_format_article_runs_hook() {
    with_test_db(|url| Box::pin(async move {
    let (state, _fl) = app(&url).await;
    // Give seed article 1's user entry a share key.
    sqlx::query("UPDATE ttrss_user_entries SET uuid = 'disp-key' WHERE ref_id = 1 AND owner_uid = 1")
        .execute(state.db.pool())
        .await
        .expect("uuid set");
    let out = public_dispatch(
        &state,
        "share",
        "get",
        &json!({ "key": "disp-key" }),
        &state.config.self_url_rtrim(),
    )
    .await
        .expect("share get");
    match out {
        ttrss::pluginhost::plugin::MethodOutput::Response { status, body, .. } => {
            assert_eq!(status, 200, "shared article page 200");
            assert!(
                body.starts_with("FMT::<!DOCTYPE html>"),
                "the HOOK_FORMAT_ARTICLE rewrite must be served: {body:?}"
            );
        }
        _ => panic!("expected an HTML response"),
    }
        Ok(())
    })).await.unwrap();
}

// ── HOOK_ARTICLES_*_TOGGLED (RPC mark/publ/markSelected/publishSelected,
//    API updateArticle, Article::_create_published_article) ──────────────

/// An admin `Sess` with the fixture pwd hash (mirrors `feed_crud.rs`).
/// `access_level` 10 = the DB admin's level (validate_session rejects a
/// stale one).
fn rpc_sess() -> ttrss::session::Sess {
    let mut d = Session::new();
    d.set("uid", PValue::Int(1));
    d.set("access_level", PValue::Int(10));
    d.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    d.set("name", PValue::Bytes(b"admin".to_vec()));
    d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let s = ttrss::session::Sess::new("rpc-sid".to_string());
    s.replace("rpc-sid".to_string(), d);
    s
}

async fn rpc_call(state: &AppState, body: &str) -> Value {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/backend.php")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .expect("request");
    let res = ttrss::rpc::handler(State(state.clone()), Extension(rpc_sess()), req).await;
    let bytes = to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    serde_json::from_str(&String::from_utf8_lossy(&bytes)).expect("json")
}

#[tokio::test]
async fn test_rpc_mark_fires_mark_toggled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            let v = rpc_call(&state, "op=RPC&method=mark&csrf_token=tok&mark=true&id=1").await;
            assert_eq!(v["message"], "UPDATE_COUNTERS", "RPC::mark ran: {v}");
            assert!(
                fl.mark_toggled.load(Ordering::SeqCst),
                "HOOK_ARTICLES_MARK_TOGGLED must fire"
            );
            assert!(
                !fl.publish_toggled.load(Ordering::SeqCst),
                "publish hook must not fire"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_rpc_publ_fires_publish_toggled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            let v = rpc_call(&state, "op=RPC&method=publ&csrf_token=tok&pub=true&id=1").await;
            assert_eq!(v["message"], "UPDATE_COUNTERS", "RPC::publ ran: {v}");
            assert!(
                fl.publish_toggled.load(Ordering::SeqCst),
                "HOOK_ARTICLES_PUBLISH_TOGGLED must fire"
            );
            assert!(
                !fl.mark_toggled.load(Ordering::SeqCst),
                "mark hook must not fire"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_rpc_mark_selected_fires_mark_toggled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            let v = rpc_call(
                &state,
                "op=RPC&method=markSelected&csrf_token=tok&ids[]=1&ids[]=2&cmode=0",
            )
            .await;
            assert_ne!(v["error"]["code"].as_str(), Some("E_UNKNOWN_METHOD"), "{v}");
            assert!(
                fl.mark_toggled.load(Ordering::SeqCst),
                "HOOK_ARTICLES_MARK_TOGGLED must fire"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_rpc_publish_selected_fires_publish_toggled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            let v = rpc_call(
                &state,
                "op=RPC&method=publishSelected&csrf_token=tok&ids[]=1&cmode=0",
            )
            .await;
            assert_ne!(v["error"]["code"].as_str(), Some("E_UNKNOWN_METHOD"), "{v}");
            assert!(
                fl.publish_toggled.load(Ordering::SeqCst),
                "HOOK_ARTICLES_PUBLISH_TOGGLED must fire"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// API `updateArticle` with `field=0` (marked) fires MARK_TOGGLED only.
#[tokio::test]
async fn test_api_update_article_marked_fires_mark() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");
            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            let sess = ttrss::session::Sess::new("api-sid".into());
            sess.replace("api-sid".into(), data);

            let (status, out) = ttrss::api::before_and_dispatch(
                &state,
                &sess,
                "updatearticle",
                json!({ "article_ids": [1], "mode": 0, "field": 0 })
                    .as_object()
                    .unwrap(),
                None,
            )
            .await;
            assert_eq!(status, 0, "updatearticle(marked) OK: {out}");
            assert!(
                fl.mark_toggled.load(Ordering::SeqCst),
                "marked → MARK_TOGGLED"
            );
            assert!(
                !fl.publish_toggled.load(Ordering::SeqCst),
                "marked must not fire publish"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// API `updateArticle` with `field=1` (published) fires PUBLISH_TOGGLED only.
#[tokio::test]
async fn test_api_update_article_published_fires_publish() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, fl) = app(&url).await;
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");
            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            let sess = ttrss::session::Sess::new("api-sid".into());
            sess.replace("api-sid".into(), data);

            let (status, out) = ttrss::api::before_and_dispatch(
                &state,
                &sess,
                "updatearticle",
                json!({ "article_ids": [1], "mode": 0, "field": 1 })
                    .as_object()
                    .unwrap(),
                None,
            )
            .await;
            assert_eq!(status, 0, "updatearticle(published) OK: {out}");
            assert!(
                fl.publish_toggled.load(Ordering::SeqCst),
                "published → PUBLISH_TOGGLED"
            );
            assert!(
                !fl.mark_toggled.load(Ordering::SeqCst),
                "published must not fire mark"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// G2: a plugin-registered API method is dispatched by the `API::index`
/// catch-all (`add_api_method` → `get_api_method` → `invoke_method`).
#[tokio::test]
async fn test_api_plugin_method_dispatched() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _fl) = app(&url).await;
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");
            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            let sess = ttrss::session::Sess::new("api-plugin-sid".into());
            sess.replace("api-plugin-sid".into(), data);

            let (status, out) = ttrss::api::before_and_dispatch(
                &state,
                &sess,
                "apimethod",
                json!({ "arg": 42 }).as_object().unwrap(),
                None,
            )
            .await;
            assert_eq!(status, 0, "API method dispatched, got {out}");
            assert_eq!(out["api"], "ok", "the plugin's method must run: {out}");
            assert_eq!(
                out["arg"], 42,
                "the request body must reach the method: {out}"
            );

            // An unregistered method is still E_UNKNOWN_METHOD.
            let (status, out) = ttrss::api::before_and_dispatch(
                &state,
                &sess,
                "nope",
                json!({}).as_object().unwrap(),
                None,
            )
            .await;
            assert_eq!(status, 1, "unknown method, got {out}");
            assert_eq!(out["error"], "UNKNOWN_METHOD");
            Ok(())
        })
    })
    .await
    .unwrap();
}
