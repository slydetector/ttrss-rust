//! DB tests for the external API's `getconfig` response shape (M10.3).
//!
//! PHP's `API::_get_config` sends **no** `translations` field — translation
//! delivery to the SPA lives in the internal RPC `init` response (see
//! `tests/rpc_init_translations.rs`), not in the external API. This file pins
//! the getconfig shape and asserts the `translations` field is absent, so a
//! stray copy of the RPC-init field can't reappear here.
//!
//! Rust-only: the frozen PHP reference has no `getconfig` test.

mod common;

use serde_json::{Map, Value};
use ttrss::api;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

/// A test-only config. `docroot` must point at the frozen `tt-rss` tree so
/// `AppState` can load the real gettext catalogs (the translations map).
fn test_config() -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = "auth_internal".into();
    cfg.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    cfg
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of all fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's body map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Enable the per-user API gate (the `before_and_dispatch` auth check).
async fn enable_api(db: &Db) {
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
}

async fn getconfig(state: &AppState, sess: &Sess) -> Value {
    let (status, content) =
        api::before_and_dispatch(state, sess, "getconfig", &request(&[]), None).await;
    assert_eq!(status, 0, "getconfig must succeed: {content}");
    content
}

/// The `getconfig` shape: the four `API::_get_config` fields are present, and
/// — crucially — there is **no** `translations` field. PHP's `API::_get_config`
/// sends none (translation delivery is the RPC `init` response); pin its
/// absence so a stray copy of that field can't reappear in the external API.
#[tokio::test]
async fn test_getconfig_shape_and_no_translations_field() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;

    let content = getconfig(&state, &sess).await;

    // The four PHP `API::_get_config` fields.
    assert_eq!(content["custom_sort_types"], Value::Array(vec![]));
    assert_eq!(content["daemon_is_running"], Value::Bool(true));
    assert!(content.get("icons_url").is_some(), "icons_url: {content}");
    assert!(content.get("num_feeds").is_some(), "num_feeds: {content}");

    // PHP sends no `translations` here.
    assert!(
        content.get("translations").is_none(),
        "getconfig must not send translations (PHP parity): {content}"
    );
        Ok(())
    })).await.unwrap();
}
