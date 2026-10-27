//! The `HOOK_RENDER_ARTICLE_API` dispatch on the API **getArticle** route
//! (PHP `API.php:360-366`). A registered stub rewrites the article row's
//! `content`; the getArticle response must carry the rewrite (before the
//! fix the route skipped the chain entirely — only getHeadlines ran it).

use crate::common;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::api;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::{About, HOOK_RENDER_ARTICLE_API, Plugin, PluginHost};
use ttrss::session::Sess;

/// Rewrites the row's `content` (a visible marker) and returns the row —
/// the PHP contract (the row, not the `params` wrapper).
struct RenderApiStub;

#[async_trait]
impl Plugin for RenderApiStub {
    fn name(&self) -> &'static str {
        "render_api_stub"
    }
    fn class_name(&self) -> &'static str {
        "Render_Api_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_RENDER_ARTICLE_API);
    }

    async fn hook_render_article_api(&self, _host: &mut HostCtx, params: &Value) -> Value {
        let mut article = params
            .get("article")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        if let Some(Value::String(content)) = article.get_mut("content") {
            let mut s = std::mem::take(content);
            s.insert_str(0, "APIHOOK ");
            *content = s;
        }
        article
    }
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

#[tokio::test]
async fn test_getarticle_runs_render_article_api_chain() {
    with_test_db(|url| {
        Box::pin(async move {
            let db: Db = common::make_db_from_url(&url).await.expect("test db");
            let mut cfg = common::test_config();
            cfg.plugins = "render_api_stub".to_string();
            let mut state = AppState::new(cfg, db);
            state.plugins = Arc::new({
                let mut host = PluginHost::new();
                host.register(Box::new(RenderApiStub));
                host
            });

            let mut data = Session::new();
            data.set("uid", PValue::Int(1));
            data.set(
                "pwd_hash",
                PValue::Bytes(crate::common::ADMIN_PWD_HASH.as_bytes().to_vec()),
            );
            data.set("access_level", PValue::Int(10));
            let sess = Sess::new("render-api-test".into());
            sess.replace("render-api-test".into(), data);

            // The per-user API gate.
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");

            // Seed article 1 (owner 1) exists in the fixture; ask for it.
            let (status, out) = api::before_and_dispatch(
                &state,
                &sess,
                "getarticle",
                &request(&[("article_id", json!([1]))]),
                None,
                false,
            )
            .await;
            assert_eq!(status, 0, "API STATUS_OK, got {out}");

            let content = out
                .pointer("/0/content")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                content.starts_with("APIHOOK "),
                "the hook's rewrite must survive into the response: {content}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
