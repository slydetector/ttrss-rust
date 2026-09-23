//! DB tests for the external API's `getconfig` locale/translations field
//! (M10.3) — regression guard for the `USER_LANGUAGE` pref read.
//!
//! The port once read the pref with the wrong column names
//! (`val_string`/`name` instead of `value`/`pref_name`); the query errored
//! on every request, the error was swallowed (`.ok().flatten()`), and the
//! user's Language setting was silently ignored on both surfaces. These
//! tests pin the pref → locale → translations flow.
//!
//! Rust-only: the frozen PHP reference has no `getconfig` test and its
//! `API::_get_config` sends no translations at all; the field is the client
//! protocol, populated only for non-English locales.

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

async fn set_user_language(db: &Db, value: &str) {
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'USER_LANGUAGE', NULL, $1)",
    )
    .bind(value)
    .execute(db.pool())
    .await
    .expect("set USER_LANGUAGE");
}

async fn getconfig(state: &AppState, sess: &Sess, accept_language: Option<&str>) -> Value {
    let (status, content) =
        api::before_and_dispatch(state, sess, "getconfig", &request(&[]), accept_language)
            .await;
    assert_eq!(status, 0, "getconfig must succeed: {content}");
    content
}

fn translations(content: &Value) -> &Map<String, Value> {
    content["translations"]
        .as_object()
        .expect("translations must be a JSON object")
}

/// The user's `USER_LANGUAGE` pref wins: German pref → a populated
/// translations map (with the German rendering of a known string) even with
/// no `Accept-Language` header.
#[tokio::test]
async fn test_getconfig_translations_follow_user_language_pref() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;
    set_user_language(&state.db, "de_DE").await;

    let content = getconfig(&state, &sess, None).await;
    let map = translations(&content);
    assert!(!map.is_empty(), "German pref must populate the translations map");
    let has_german = map.values().any(|v| v.as_str() == Some("Ladevorgang, bitte warten..."));
    assert!(has_german, "map should carry the German 'Loading, please wait...': {map:?}");
        Ok(())
    })).await.unwrap();
}

/// No pref, no header → English → the map is present but empty.
#[tokio::test]
async fn test_getconfig_translations_empty_for_english() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;

    let content = getconfig(&state, &sess, None).await;
    assert!(translations(&content).is_empty(), "English must give an empty map");
        Ok(())
    })).await.unwrap();
}

/// No pref → the `Accept-Language` header is the fallback (the pre-existing
/// path that must keep working).
#[tokio::test]
async fn test_getconfig_translations_fall_back_to_accept_language() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;

    let content = getconfig(&state, &sess, Some("de-DE")).await;
    assert!(!translations(&content).is_empty(), "de-DE header must populate the map");
        Ok(())
    })).await.unwrap();
}
