//! Parity tests for the `op=forgotpass` password-recovery flow (a faithful port
//! of `Handler_Public::forgotpass()`).
//!
//! These drive the handler directly (no HTTP) against a fresh Postgres fixture:
//! the request-form GET, the `method=do` POST (CSRF + arithmetic CAPTCHA), and
//! the reset-link GET (15-hour token validation + password reset).
mod common;

use axum::body::to_bytes;
use axum::response::Response;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::forgotpass;
use ttrss::session::Sess;
use ttrss::AppState;

fn test_config() -> Config {
    Config {
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
        docroot: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tt-rss"),
    }
}

async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState {
        config: test_config(),
        db,
    }
}

/// Read an axum `Response` body to a UTF-8 string.
async fn body_of(res: Response) -> String {
    let bytes = to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

/// The user's stored `resetpass_token` (`None` if cleared).
async fn stored_token(pool: &sqlx::PgPool, login: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT resetpass_token FROM ttrss_users WHERE login = $1",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
    .expect("token query")
    .flatten()
}

/// The complete flow: request form → validated POST (token stored) → reset link
/// (password reset, token cleared).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_forgotpass_full_flow(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options).await;
    let p = state.db.pool();

    // The `admin` fixture user has email = ''; seed one with a real email.
    sqlx::query(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level, email) \
         VALUES ('rpuser', 'x', 0, 'rpuser@example.com')",
    )
    .execute(p)
    .await
    .expect("seed user");

    let sess = Sess::new(String::new()); // public (non-logged-in) session

    // Step 1: the request form (no `method`, no `hash`).
    let html = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: None,
                login: "",
                email: None,
                test: None,
                csrf_token: None,
                method: None,
            },
        )
        .await,
    )
    .await;
    assert!(
        html.contains("Password reset link will be sent"),
        "intro missing"
    );
    for field in [
        "name='login'",
        "name='email'",
        "name='test'",
        "name='csrf_token'",
    ] {
        assert!(html.contains(field), "form field {field:?} missing");
    }
    assert!(!sess.id().is_empty(), "handler should mint a session id");

    let csrf = sess
        .data()
        .csrf_token()
        .expect("csrf token set")
        .to_string();
    let v1 = sess
        .data()
        .get_int("pwdreset:testvalue1")
        .expect("testvalue1");
    let v2 = sess
        .data()
        .get_int("pwdreset:testvalue2")
        .expect("testvalue2");

    // Step 2a: a POST with a wrong CSRF token is rejected (and no token stored).
    let sum = (v1 + v2).to_string();
    let html = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: None,
                login: "rpuser",
                email: Some("rpuser@example.com"),
                test: Some(sum.as_str()),
                csrf_token: Some("wrong"),
                method: Some("do"),
            },
        )
        .await,
    )
    .await;
    assert!(
        html.contains("required form parameters are missing or incorrect"),
        "bad csrf should be rejected; got: {html}"
    );
    assert!(stored_token(p, "rpuser").await.is_none());

    // Step 2b: a correct POST stores the reset token.
    let html = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: None,
                login: "rpuser",
                email: Some("rpuser@example.com"),
                test: Some(sum.as_str()),
                csrf_token: Some(csrf.as_str()),
                method: Some("do"),
            },
        )
        .await,
    )
    .await;
    assert!(
        html.contains("Password reset instructions are being sent"),
        "got: {html}"
    );
    let tok = stored_token(p, "rpuser").await.expect("token stored");
    let (ts, hash) = tok.split_once(':').expect("time:token shape");
    assert!(!ts.is_empty() && !hash.is_empty());

    // Step 3: following the reset link resets the password + clears the token.
    let html = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: Some(hash),
                login: "rpuser",
                email: None,
                test: None,
                csrf_token: None,
                method: None,
            },
        )
        .await,
    )
    .await;
    assert!(html.contains("Completed."), "got: {html}");
    assert!(html.contains("Changed password of user"), "got: {html}");
    assert!(stored_token(p, "rpuser").await.is_none(), "token cleared");
}

/// A POST for an unknown login reports "not found" and stores no token.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_forgotpass_unknown_user(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options).await;
    let p = state.db.pool();

    let sess = Sess::new(String::new());
    // Mint the form (to populate the CAPTCHA + csrf).
    let _ = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: None,
                login: "",
                email: None,
                test: None,
                csrf_token: None,
                method: None,
            },
        )
        .await,
    )
    .await;
    let csrf = sess.data().csrf_token().expect("csrf").to_string();
    let v1 = sess.data().get_int("pwdreset:testvalue1").expect("v1");
    let v2 = sess.data().get_int("pwdreset:testvalue2").expect("v2");
    let sum = (v1 + v2).to_string();

    let html = body_of(
        forgotpass::forgotpass(
            &state,
            &sess,
            &forgotpass::Params {
                hash: None,
                login: "nobody",
                email: Some("nobody@example.com"),
                test: Some(sum.as_str()),
                csrf_token: Some(csrf.as_str()),
                method: Some("do"),
            },
        )
        .await,
    )
    .await;
    assert!(
        html.contains("login and email combination not found"),
        "got: {html}"
    );
    assert!(stored_token(p, "nobody").await.is_none());
}

/// `disable_login_form` (or a missing `auth_internal` plugin) → 403.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_forgotpass_forbidden(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let mut config = test_config();
    config.disable_login_form = true;
    let state = AppState { config, db };
    let sess = Sess::new(String::new());

    let res = forgotpass::forgotpass(
        &state,
        &sess,
        &forgotpass::Params {
            hash: None,
            login: "",
            email: None,
            test: None,
            csrf_token: None,
            method: None,
        },
    )
    .await;
    assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
}
