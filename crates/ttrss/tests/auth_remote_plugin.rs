//! M7.20 — the **`auth_remote`** plugin (the last first-party plugin;
//! completes Phase 7).
//!
//! PHP reference: `tt-rss/plugins/auth_remote/init.php`. The login path is
//! pluggable (the `HOOK_AUTH_USER` chain over the **system** plugins —
//! PHP `load_all(KIND_SYSTEM, 0, true)`); `auth_remote` authenticates
//! against proxy-supplied credentials instead of the form:
//!
//! - the proxy user (the first non-empty of `REMOTE_USER`,
//!   `HTTP_REMOTE_USER`, `REDIRECT_REMOTE_USER`, `PHP_AUTH_USER` — the last
//!   one the port synthesizes from the `Authorization: Basic` header), else
//!   the client-SSL certificate's registered `SSL_CERT_SERIAL` pref;
//! - the account is auto-created when missing (`AUTH_AUTO_CREATE`, default
//!   on; a random password) and the LemonLDAP name/email headers update it;
//! - on success the session carries the `fake_login`/`fake_password`/
//!   `hide_hello`/`hide_logout` markers (the shell hides the Logout item
//!   for those sessions);
//! - with `AUTH_REMOTE_POST_LOGOUT_URL` set, the `HOOK_POST_LOGOUT` chain
//!   redirects the logout to that URL (validated; empty/invalid → the
//!   default index).

mod common;

use axum::body::Body;

/// Init a tracing subscriber once per test process (the binary's
/// `main` does this; test binaries don't run `main`).
fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .try_init();
    });
}
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use ttrss::http;
use ttrss::AppState;
use test_db::template::with_test_db;

async fn make_state(url: &str, mut mutate: impl FnMut(&mut ttrss::core::config::Config)) -> AppState {
    init_tracing();
    let db = common::make_db_from_url(url).await.expect("test db");
    let mut cfg = common::test_config();
    // M7.20: the auth chain runs over the **system** plugins (PHP
    // `load_all(KIND_SYSTEM, 0, true)`) — `TTRSS_PLUGINS` does not gate it.
    mutate(&mut cfg);
    AppState::new(cfg, db)
}

fn app(state: AppState) -> Router {
    http::build_router(&state).with_state(state)
}

fn post_form(form: &str, extra: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/tt-rss/public.php")
        .header("content-type", "application/x-www-form-urlencoded");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    req.body(Body::from(form.to_string())).expect("request")
}

fn set_cookie_of(resp: &axum::response::Response) -> String {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .find(|s| s.starts_with("ttrss_sid="))
        .expect("session cookie")
}

async fn get_index(
    state: &AppState,
    cookie: &str,
) -> (StatusCode, String, Option<String>) {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss/index.php")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("request");
    let resp = app(state.clone()).oneshot(req).await.expect("response");
    let status = resp.status();
    let etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string(), etag)
}

/// The proxy-login login form (the proxy headers decide, not the form:
/// PHP's `login`/`password` are only the user's typed values, ignored for
/// auth_remote; the login op has no CSRF gate in PHP).
fn proxy_login_form(login: &str, password: &str) -> String {
    format!("op=login&login={login}&password={password}")
}

// ── the proxy user (REMOTE_USER) ─────────────────────────────────────────

#[tokio::test]
async fn test_remote_user_auto_creates_and_fake_login_session() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("bob", "ignored"),
            &[("REMOTE_USER", "Bob")],
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND, "proxy login must redirect");
    assert!(resp.headers().get(header::LOCATION).is_some());
    let cookie = set_cookie_of(&resp);
    // The account was auto-created (lowercased, random password).
    let row: Option<(i32, i32, String)> = sqlx::query_as(
        "SELECT id, access_level, pwd_hash FROM ttrss_users WHERE login = 'bob'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("query");
    let (uid, access, hash) = row.expect("bob auto-created");
    assert_eq!(access, 0, "auto-created users are plain users");
    assert!(hash.starts_with("SSHA-512:"), "auto-created with SSHA-512");
    assert_ne!(hash.split_once(':').unwrap().1, "ignored", "the form password must not be the stored one when auto-created with an empty seed");
    // The session carries the fake-login markers.
    let data = read_session(&state, &cookie).await;
    assert_eq!(data.get_str("fake_login"), Some("bob"));
    assert_eq!(data.get_str("fake_password"), Some("******"));
    assert_eq!(data.get_bool("hide_logout"), Some(true));
    assert_eq!(data.get_bool("hide_hello"), Some(true));
    assert_eq!(data.uid(), Some(uid as i64));
    assert_eq!(
        data.get_str("auth_module"),
        Some("Auth_Remote"),
        "the authenticating plugin's class is recorded"
    );

    // The shell hides the Logout item for fake-login sessions.
    let (status, html, _) = get_index(&state, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.contains("qmcLogout"), "hide_logout must drop the Logout item");
    Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_remote_user_updates_lemonldap_name_email() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    // First login creates the account + the name/email.
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("carol", ""),
            &[("REMOTE_USER", "carol"), ("HTTP_USER_NAME", "Carol Danvers"), ("HTTP_USER_MAIL", "carol@example.com")],
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND);
    // A second login (existing account) updates the name/email again.
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("carol", ""),
            &[("REMOTE_USER", "carol"), ("AUTHENTICATE_CN", "Carol X"), ("AUTHENTICATE_MAIL", "carol2@example.com")],
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let row: (String, String) = sqlx::query_as(
        "SELECT full_name, email FROM ttrss_users WHERE login = 'carol'",
    )
    .fetch_one(state.db.pool())
    .await
    .expect("carol row");
    assert_eq!(row.0, "Carol X", "AUTHENTICATE_CN wins over the first login's value");
    assert_eq!(row.1, "carol2@example.com");
    Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_basic_auth_header_maps_to_php_auth_user() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    let basic = base64_encode("dave:anypass");
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("dave", ""),
            &[("Authorization", &format!("Basic {basic}"))],
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND, "Basic auth must log in");
    let exists: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_users WHERE login = 'dave'")
            .fetch_optional(state.db.pool())
            .await
            .expect("query");
    assert!(exists.is_some(), "dave auto-created via PHP_AUTH_USER");
    Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_no_proxy_credentials_falls_through_to_auth_internal() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    // A real password account.
    let salt = "testsalt";
    let inner = ttrss::core::user::hash_password(
        "evepass",
        salt,
        ttrss::core::user::ALGO_SSHA512,
    )
    .expect("hash");
    sqlx::query(
        "INSERT INTO ttrss_users (login, salt, pwd_hash, access_level) VALUES ('eve', $1, $2, 0)",
    )
    .bind(salt)
    .bind(format!("{inner}:{salt}"))
    .execute(state.db.pool())
    .await
    .expect("eve insert");
    // No proxy headers: auth_remote fails, auth_internal verifies the
    // password (the port's login flow's fallback — PHP's chain does the
    // same, auth_internal is also system).
    let resp = app(state.clone())
        .oneshot(post_form("op=login&login=eve&password=evepass", &[]))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND, "password login must succeed");
    Ok(())
    })).await.unwrap();
}

// ── the SSL-certificate path ─────────────────────────────────────────────

#[tokio::test]
async fn test_ssl_certificate_serial_maps_to_registered_user() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    // A user with a registered cert serial pref (the fingerprint PHP
    // computes from the mod_ssl headers).
    let salt = "testsalt";
    let inner = ttrss::core::user::hash_password("gracepass", salt, ttrss::core::user::ALGO_SSHA512).unwrap();
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, salt, pwd_hash, access_level) VALUES ('grace', $1, $2, 0) RETURNING id",
    )
    .bind(salt)
    .bind(format!("{inner}:{salt}"))
    .fetch_one(state.db.pool())
    .await
    .expect("grace insert");
    let serial = ttrss::util::sha1_hex("1A2B3C:2026:01:01:2026:12:31:C=US,O=TT");
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, value) VALUES ($1, 'SSL_CERT_SERIAL', $2)",
    )
    .bind(uid)
    .bind(&serial)
    .execute(state.db.pool())
    .await
    .expect("serial pref");
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("grace", ""),
            &[
                ("SSL_CLIENT_M_SERIAL", "1A2B3C"),
                ("SSL_CLIENT_V_START", "2026:01:01"),
                ("SSL_CLIENT_V_END", "2026:12:31"),
                ("SSL_CLIENT_S_DN", "C=US,O=TT"),
            ],
        ))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND, "cert login must succeed");
    Ok(())
    })).await.unwrap();
}

// ── the post-logout redirect (HOOK_POST_LOGOUT) ──────────────────────────

#[tokio::test]
async fn test_logout_redirects_to_post_logout_url_when_configured() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |cfg| {
        cfg.auth_remote_post_logout_url = "https://idp.example/logout-redirect".into();
    }).await;
    // Log in via the proxy (a fake-login session).
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("hank", ""),
            &[("REMOTE_USER", "hank")],
        ))
        .await
        .expect("response");
    let cookie = set_cookie_of(&resp);
    let csrf = read_session(&state, &cookie).await.csrf_token().unwrap_or("").to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/tt-rss/public.php")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(header::COOKIE, cookie)
        .body(Body::from(format!("op=logout&csrf_token={csrf}")))
        .expect("request");
    let resp = app(state).oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let loc = resp.headers().get(header::LOCATION).unwrap().to_str().unwrap();
    assert_eq!(loc, "https://idp.example/logout-redirect", "the chain's validated URL wins");
    Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_logout_defaults_to_index_when_url_unset() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |_| {}).await;
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("iris", ""),
            &[("REMOTE_USER", "iris")],
        ))
        .await
        .expect("response");
    let cookie = set_cookie_of(&resp);
    let csrf = read_session(&state, &cookie).await.csrf_token().unwrap_or("").to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/tt-rss/public.php")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(header::COOKIE, cookie)
        .body(Body::from(format!("op=logout&csrf_token={csrf}")))
        .expect("request");
    let resp = app(state).oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let loc = resp.headers().get(header::LOCATION).unwrap().to_str().unwrap();
    assert!(loc.ends_with("/index.php"), "no URL configured → the default index");
    Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_logout_ignores_invalid_post_logout_url() {
    with_test_db(|url| Box::pin(async move {
    let state = make_state(&url, |cfg| {
        cfg.auth_remote_post_logout_url = "javascript:alert(1)".into();
    }).await;
    let resp = app(state.clone())
        .oneshot(post_form(
            &proxy_login_form("jack", ""),
            &[("REMOTE_USER", "jack")],
        ))
        .await
        .expect("response");
    let cookie = set_cookie_of(&resp);
    let csrf = read_session(&state, &cookie).await.csrf_token().unwrap_or("").to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/tt-rss/public.php")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(header::COOKIE, cookie)
        .body(Body::from(format!("op=logout&csrf_token={csrf}")))
        .expect("request");
    let resp = app(state).oneshot(req).await.expect("response");
    let loc = resp.headers().get(header::LOCATION).unwrap().to_str().unwrap();
    assert!(loc.ends_with("/index.php"), "UrlHelper::validate rejects non-http(s)");
    Ok(())
    })).await.unwrap();
}

// ── helpers ──────────────────────────────────────────────────────────────

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// Read a stored session back (the app's own decode path: `from_stored`
/// does the base64 + envelope + phpser parse).
async fn read_session(state: &AppState, cookie: &str) -> ttrss::core::session::Session {
    // The cookie value is `id; path=/` (PHP-style; the middleware's
    // `cookie_sid` strips at the first `;`).
    let sid = cookie
        .strip_prefix("ttrss_sid=")
        .unwrap_or(cookie)
        .split(';')
        .next()
        .unwrap_or("")
        .to_string();
    let stored: Option<String> =
        sqlx::query_scalar("SELECT data FROM ttrss_sessions WHERE id = $1")
            .bind(&sid)
            .fetch_optional(state.db.pool())
            .await
            .expect("session query");
    let stored = match stored {
        Some(row) => row,
        None => panic!("session row missing for {sid}"),
    };
    ttrss::core::session::from_stored(
        &stored,
        state.config.encryption_key.as_ref(),
    )
    .expect("session payload")
}
