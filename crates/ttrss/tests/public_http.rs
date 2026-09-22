//! HTTP-level parity tests for the `public.php` handler (`ttrss::http`) — the
//! axum layer that the fn-direct tests in `public_getprofiles.rs` bypass.
//!
//! Regression guard: the login page's Profile dropdown POSTs
//! `op=getprofiles&login=…` in the **form body** (no query string). PHP reads
//! `$_REQUEST["login"]` (GET + POST merged); the port must accept the param
//! from either, otherwise the dropdown degrades to an empty disabled select
//! (the response for a missing login is `[]`, which wipes the placeholder).

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use serde_json::Value;
use ttrss::http;
use ttrss::AppState;
use test_db::template::with_test_db;

async fn make_state(url: &str) -> AppState {
    let db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(common::test_config(), db)
}

/// The router as the binary builds it (state consumed by `with_state`).
fn app(state: AppState) -> Router {
    http::build_router(&state).with_state(state)
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("response is JSON")
}

/// Insert a user + profiles; returns the user id.
async fn add_user_with_profiles(state: &AppState, login: &str, profiles: &[&str]) -> i32 {
    let pool = state.db.pool();
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level) VALUES ($1, 'SHA1:x', 0) RETURNING id",
    )
    .bind(login)
    .fetch_one(pool)
    .await
    .expect("user insert");
    for title in profiles {
        sqlx::query("INSERT INTO ttrss_settings_profiles (owner_uid, title) VALUES ($1, $2)")
            .bind(uid)
            .bind(title)
            .execute(pool)
            .await
            .expect("profile insert");
    }
    uid
}

/// POST form body to `/tt-rss/public.php` (the login page's actual request
/// shape: params in the body, none in the query string).
fn post_form(form: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/tt-rss/public.php")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_string()))
        .expect("request")
}

/// The expected response for `login`: `Default profile` first, then the
/// user's profiles title-asc, ids as `value`.
async fn expect_profiles(state: &AppState, v: &serde_json::Value, login: &str) {
    let uid: i32 = sqlx::query_scalar("SELECT id FROM ttrss_users WHERE login = $1")
        .bind(login)
        .fetch_one(state.db.pool())
        .await
        .expect("user row");
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT id, title FROM ttrss_settings_profiles WHERE owner_uid = $1 ORDER BY title ASC",
    )
    .bind(uid)
    .fetch_all(state.db.pool())
    .await
    .expect("profile rows");
    let expected: Vec<serde_json::Value> = std::iter::once(serde_json::json!({
        "label": "Default profile",
        "value": 0,
    }))
    .chain(
        rows.into_iter()
            .map(|(id, title)| serde_json::json!({ "label": title, "value": id })),
    )
    .collect();
    assert_eq!(v, &serde_json::json!(expected), "login={login:?}");
}

#[tokio::test]
async fn test_getprofiles_form_post_populates_dropdown() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    // Inserted out of order; the response must be title-ascending.
    add_user_with_profiles(&state, "httpprofile", &["Work", "Home"]).await;
    let app = app(state.clone());

    let resp = app
        .oneshot(post_form("op=getprofiles&login=httpprofile"))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "form POST must be 200");
    let v = body_json(resp).await;
    expect_profiles(&state, &v, "httpprofile").await;
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_getprofiles_query_string_still_works() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    add_user_with_profiles(&state, "queryprofile", &["Solo"]).await;
    let app = app(state.clone());

    let req = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss/public.php?op=getprofiles&login=queryprofile")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    expect_profiles(&state, &v, "queryprofile").await;
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_getprofiles_form_post_missing_login_returns_empty() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    let app = app(state);

    // `$_REQUEST["login"]` unset in either layer -> PHP `if ($login)` is
    // false -> `[]` (the dropdown keeps its placeholder; the JS only fires
    // with a non-empty value anyway).
    let resp = app
        .oneshot(post_form("op=getprofiles&login="))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, serde_json::json!([]));
        Ok(())
    })).await.unwrap();
}

// ── M5a.1: the client directory-URL API routes (`POST /api/`, `POST /api`) ──

/// Insert a user with a valid app password for the `_api` service (the
/// external-API credential, `ALGO:HASH:SALT`); returns the login name.
async fn add_user_with_api_password(state: &AppState) -> &'static str {
    let pool = state.db.pool();
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level) VALUES ($1, 'SHA1:x', 0) RETURNING id",
    )
    .bind("apilogin")
    .fetch_one(pool)
    .await
    .expect("user insert");
    let salt = "testsalt";
    let inner = ttrss::core::user::hash_password(
        "apipass",
        salt,
        ttrss::core::user::ALGO_SSHA512,
    )
    .expect("hash");
    sqlx::query(
        "INSERT INTO ttrss_app_passwords (title, owner_uid, pwd_hash, service, created) \
         VALUES ('t', $1, $2, '_api', NOW())",
    )
    .bind(uid)
    .bind(format!("{inner}:{salt}"))
    .execute(pool)
    .await
    .expect("app password insert");
    // PHP's `ENABLE_API_ACCESS` pref defaults to **false** — the user must
    // have enabled API access for `API::login` to succeed.
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, value) \
         VALUES ($1, 'ENABLE_API_ACCESS', 't')",
    )
    .bind(uid)
    .execute(pool)
    .await
    .expect("pref insert");
    "apilogin"
}

/// POST a JSON body to the API at the given path (the client's request shape:
/// JSON body, params as keys, no query string).
fn post_json_api(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// The successful-login envelope: `status 0` with `session_id` + `api_level`
/// (the two fields the client stores after login).
async fn expect_api_login_envelope(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "application/json"
    );
    let v = body_json(resp).await;
    assert_eq!(v.get("status").and_then(Value::as_i64), Some(0), "envelope={v:?}");
    let content = v.get("content").expect("content");
    assert!(
        content
            .get("session_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "session_id={content:?}"
    );
    assert_eq!(content.get("api_level").and_then(Value::as_i64), Some(23));
}

#[tokio::test]
async fn test_api_login_via_directory_url() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    add_user_with_api_password(&state).await;
    let app = app(state);

    // `ApiCommon.performRequest`: `<ttrss_url>/api/` (trailing slash, no
    // index.php) — the client's actual endpoint.
    let resp = app
        .oneshot(post_json_api(
            "/tt-rss/api/",
            r#"{"op":"login","user":"apilogin","password":"apipass"}"#,
        ))
        .await
        .expect("response");
    expect_api_login_envelope(resp).await;
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_api_login_via_bare_path() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    add_user_with_api_password(&state).await;
    let app = app(state);

    // The slash-less form: served directly (no Apache 301).
    let resp = app
        .oneshot(post_json_api(
            "/tt-rss/api",
            r#"{"op":"login","user":"apilogin","password":"apipass"}"#,
        ))
        .await
        .expect("response");
    expect_api_login_envelope(resp).await;
        Ok(())
    })).await.unwrap();
}

/// Insert a user with a *main* (web) password and **no** `_api` app password;
/// returns the login name. Exercises the regular-password fallback in
/// `API::login` (and the OTP gate that disables it).
async fn add_user_with_main_password(
    state: &AppState,
    login: &'static str,
    password: &str,
    otp_enabled: bool,
) -> &'static str {
    let pool = state.db.pool();
    let salt = "mainsalt";
    let pwd_hash =
        ttrss::core::user::hash_password(password, salt, ttrss::core::user::ALGO_SSHA512)
            .expect("hash");
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level, otp_enabled) \
         VALUES ($1, $2, $3, 0, $4) RETURNING id",
    )
    .bind(login)
    .bind(&pwd_hash)
    .bind(salt)
    .bind(otp_enabled)
    .fetch_one(pool)
    .await
    .expect("user insert");
    // `ENABLE_API_ACCESS` defaults to false; enable it so `API::login` proceeds.
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, value) \
         VALUES ($1, 'ENABLE_API_ACCESS', 't')",
    )
    .bind(uid)
    .execute(pool)
    .await
    .expect("pref insert");
    login
}

/// `API::login` (service `_api`): a user with OTP **off** and no matching app
/// password authenticates via the *regular* password fallback — the `admin/admin`
/// scenario the Android client hits.
#[tokio::test]
async fn test_api_login_main_password_fallback() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    let login = add_user_with_main_password(&state, "mainlogin", "mainpass", false).await;
    let app = app(state);

    let resp = app
        .oneshot(post_json_api(
            "/tt-rss/api/",
            &format!(r#"{{"op":"login","user":"{login}","password":"mainpass"}}"#),
        ))
        .await
        .expect("response");
    expect_api_login_envelope(resp).await;
        Ok(())
    })).await.unwrap();
}

/// The fallback must NOT apply to OTP-enabled accounts: with OTP on and no
/// app password, the regular password is rejected (app passwords only).
#[tokio::test]
async fn test_api_login_otp_enabled_requires_app_password() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    let login = add_user_with_main_password(&state, "otplogin", "otppass", true).await;
    let app = app(state);

    let resp = app
        .oneshot(post_json_api(
            "/tt-rss/api/",
            &format!(r#"{{"op":"login","user":"{login}","password":"otppass"}}"#),
        ))
        .await
        .expect("response");
    let v = body_json(resp).await;
    assert_eq!(v.get("status").and_then(Value::as_i64), Some(1), "envelope={v:?}");
    assert_eq!(
        v.get("content").and_then(|c| c.get("error")).and_then(Value::as_str),
        Some("LOGIN_ERROR"),
        "envelope={v:?}"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_base_path_redirects_to_trailing_slash() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = make_state(&url).await;
    let app = app(state);

    // GET /tt-rss (no trailing slash) must 301 → /tt-rss/
    let req = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::MOVED_PERMANENTLY,
        "base path without slash must 301"
    );
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert_eq!(location, "/tt-rss/", "redirect target");
        Ok(())
    })).await.unwrap();
}
