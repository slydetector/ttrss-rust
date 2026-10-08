//! The `Feeds::view` plugin hooks (G1 wiring):
//! `HOOK_HEADLINES_BEFORE` (first-page string prepended to `content`),
//! `HOOK_HEADLINE_TOOLBAR_BUTTON` / `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2`
//! (toolbar `plugin_buttons` / `plugin_menu_items` slots), and
//! `HOOK_QUERY_HEADLINES` (per-row rewrite before the row is built).
//!
//! No first-party plugin implements these, so the dispatch sites are no-ops
//! for the stock set (exactly as in PHP). These tests prove the *dispatch*
//! itself through a registered stub.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::{
    About, HOOK_HEADLINE_TOOLBAR_BUTTON, HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2,
    HOOK_HEADLINES_BEFORE, HOOK_QUERY_HEADLINES, Plugin, PluginHost,
};
use ttrss::session::Sess;

struct ViewStub;

#[async_trait]
impl Plugin for ViewStub {
    fn name(&self) -> &'static str {
        "view_stub"
    }
    fn class_name(&self) -> &'static str {
        "View_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_HEADLINES_BEFORE);
        host.add_hook_default(HOOK_HEADLINE_TOOLBAR_BUTTON);
        host.add_hook_default(HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2);
        host.add_hook_default(HOOK_QUERY_HEADLINES);
    }

    async fn hook_headlines_before(
        &self,
        _host: &mut HostCtx,
        _feed: i32,
        _is_cat: bool,
        _qfh_ret: &Value,
    ) -> String {
        "<b>BEFORE</b>".to_string()
    }

    async fn hook_headline_toolbar_button(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _is_cat: bool,
    ) -> String {
        "<span>TB</span>".to_string()
    }

    async fn hook_headline_toolbar_select_menu_item2(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _is_cat: bool,
    ) -> String {
        "<option>MI</option>".to_string()
    }

    async fn hook_query_headlines(
        &self,
        _host: &mut HostCtx,
        row: &mut Value,
        _excerpt_length: i32,
    ) -> Value {
        if let Some(Value::String(t)) = row.get_mut("title") {
            t.push_str(" [QH]");
        }
        row.clone()
    }
}

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
    let s = Sess::new("view-hooks".into());
    s.replace("view-hooks".into(), d);
    s
}

#[tokio::test]
async fn test_view_runs_headline_toolbar_hooks() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await.expect("test db");
            let mut cfg = common::test_config();
            cfg.plugins = "view_stub".to_string();
            let mut state = AppState::new(cfg, db);
            state.plugins = Arc::new({
                let mut host = PluginHost::new();
                host.register(Box::new(ViewStub));
                host
            });

            let request = axum::http::Request::builder()
                .method(axum::http::Method::POST)
                .uri("/backend.php")
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(axum::body::Body::from(
                    "op=Feeds&method=view&feed=1&csrf_token=tok".to_string(),
                ))
                .unwrap();
            let res = ttrss::rpc::handler(
                axum::extract::State(state.clone()),
                axum::extract::Extension(sess()),
                request,
            )
            .await;
            let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024)
                .await
                .unwrap();
            let v: Value = serde_json::from_slice(&bytes).unwrap();

            // `content` is the stub's string followed by the article rows.
            let content = v["headlines"]
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(!content.is_empty(), "expected a content array: {v}");
            assert_eq!(
                content[0],
                json!("<b>BEFORE</b>"),
                "hook string must be first"
            );

            // Toolbar slots carry the hook outputs.
            assert_eq!(
                v["headlines"]["toolbar"]["plugin_buttons"],
                json!("<span>TB</span>")
            );
            assert_eq!(
                v["headlines"]["toolbar"]["plugin_menu_items"],
                json!("<option>MI</option>")
            );

            // The per-row rewrite flows into the served row.
            let first_title = content
                .get(1)
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                first_title.ends_with(" [QH]"),
                "the HOOK_QUERY_HEADLINES title rewrite must be served: {first_title}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
