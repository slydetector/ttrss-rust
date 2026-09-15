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
use ttrss::core::db::Db;
use ttrss::http;
use ttrss::AppState;

fn test_config() -> ttrss::core::config::Config {
    ttrss::core::config::Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        db_max_connections: 10,
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_max_jobs: 2,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

async fn make_state(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState {
        config: test_config(),
        db,
    }
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

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_form_post_populates_dropdown(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
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
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_query_string_still_works(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
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
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_form_post_missing_login_returns_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
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
}
