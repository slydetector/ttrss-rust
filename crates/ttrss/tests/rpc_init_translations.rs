//! DB test for the RPC `init` (`sanitycheck`) `translations` field (M10.2) —
//! the SPA's client-side translation source (`App._translations`, `App.js:624`).
//!
//! Regression guard for the empty-placeholder bug: the field was hardcoded to
//! `Value::Array(vec![])` (present but empty, and the wrong type), so
//! client-side translation was dead. PHP populates it in `RPC::init`
//! (`RPC.php:198`) — an original→translated object for the resolved locale,
//! empty for English.
//!
//! Rust-only: the frozen PHP suite has no `init`-translations test.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::AppState;
use test_db::template::with_test_db;
use tower::ServiceExt;

/// A test-only config. `docroot` must point at the frozen `tt-rss` tree so
/// `AppState` can load the real gettext catalogs.
fn test_config() -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = "auth_internal".into();
    cfg.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    cfg
}

async fn setup(url: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(), db)
}

/// Persist a logged-in session (uid 1, a CSRF token) and GET the `init`
/// (`sanitycheck`) RPC with its cookie. Returns the decoded JSON response.
async fn init_rpc(state: &AppState, accept_language: Option<&str>) -> Value {
    // uid 1 (the fixture admin) with its stored `pwd_hash` + a CSRF token, so
    // `validate_session` + the CSRF gate pass and the test is the routing.
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(100));
    data.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    ttrss::session::save(&state.db, None, "test-sid", &data)
        .await
        .expect("session save");
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(
            "/tt-rss/backend.php?op=RPC&method=sanitycheck&csrf_token=tok\
             &clientLocation=http%3A%2F%2F127.0.0.1%3A8280%2Ftt-rss%2Findex.php",
        )
        .header("cookie", "ttrss_sid=test-sid");
    if let Some(lang) = accept_language {
        builder = builder.header("accept-language", lang);
    }
    let req = builder.body(Body::empty()).expect("request");
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "init must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("init response must be JSON")
}

/// The `translations` field is an object (the original→translated map), not an
/// array — pin the type as well as the content.
fn translations(v: &Value) -> &serde_json::Map<String, Value> {
    v["translations"]
        .as_object()
        .expect("translations must be a JSON object")
}

/// The user's `USER_LANGUAGE` pref populates the map (a real German rendering
/// of a known string) even with no `Accept-Language` header.
#[tokio::test]
async fn test_init_translations_follow_user_language_pref() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'USER_LANGUAGE', NULL, 'de_DE')",
    )
    .execute(state.db.pool())
    .await
    .expect("set USER_LANGUAGE");

    let content = init_rpc(&state, None).await;
    let map = translations(&content);
    assert!(!map.is_empty(), "German pref must populate the translations map");
    let has_german = map.values().any(|v| v.as_str() == Some("Ladevorgang, bitte warten..."));
    assert!(has_german, "map should carry the German 'Loading, please wait...': {map:?}");
        Ok(())
    })).await.unwrap();
}

/// No pref, no header → English → the object is present but empty.
#[tokio::test]
async fn test_init_translations_empty_for_english() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    let content = init_rpc(&state, None).await;
    assert!(
        translations(&content).is_empty(),
        "English must give an empty map"
    );
        Ok(())
    })).await.unwrap();
}

/// No pref → the `Accept-Language` header is the fallback.
#[tokio::test]
async fn test_init_translations_fall_back_to_accept_language() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    let content = init_rpc(&state, Some("de-DE")).await;
    assert!(!translations(&content).is_empty(), "de-DE header must populate the map");
        Ok(())
    })).await.unwrap();
}

/// GET the `view` RPC (Feeds/view) with the given `feed`, using the same
/// logged-in session as [`init_rpc`]. Returns the decoded body as a string
/// (the "not found" error string lands in `headlines.content`).
async fn view_rpc(state: &AppState, feed: &str) -> String {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(100));
    data.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    ttrss::session::save(&state.db, None, "view-sid", &data)
        .await
        .expect("session save");
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/tt-rss/backend.php?op=Feeds&method=view&csrf_token=tok\
             &clientLocation=http%3A%2F%2F127.0.0.1%3A8280%2Ftt-rss%2Findex.php&feed={feed}"
        ))
        .header("cookie", "ttrss_sid=view-sid")
        .body(Body::empty())
        .expect("request");
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "view must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// M10.3: the SPA `view` *surface* carries the resolved UI locale. A `de_DE`
/// user viewing a nonexistent feed gets the German "Feed nicht gefunden."
/// (the M10.2 `init` tests prove the catalog machinery; this proves the `view`
/// path is wired to it — the linchpin M10.6's multi-locale e2e builds on).
#[tokio::test]
async fn test_view_feed_not_found_translated() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'USER_LANGUAGE', NULL, 'de_DE')",
    )
    .execute(state.db.pool())
    .await
    .expect("set USER_LANGUAGE");

    let body = view_rpc(&state, "999999").await;
    assert!(
        body.contains("Feed nicht gefunden."),
        "de_DE user must get the German 'Feed not found.'; got: {body}"
    );
        Ok(())
    })).await.unwrap();
}
