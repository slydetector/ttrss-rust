//! Parity tests for `Handler_Public::getProfiles` (the `op=getprofiles`
//! public.php endpoint the login page's Profile dropdown fetches) — a faithful
//! port of the PHP method: `[{label, value}, …]` with the `Default profile`
//! entry first, profiles title-asc, case-insensitive login match.
//!
//! Drives `ttrss::auth::get_profiles` directly against a fresh Postgres
//! fixture. The axum layer above it (`ttrss::http`) is covered at the HTTP
//! level in `public_http.rs` (params from the form body, as the login page
//! sends them).

mod common;

use ttrss::auth;
use ttrss::core::db::Db;
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

/// Insert a user + profiles; returns the user id.
async fn add_user_with_profiles(
    state: &AppState,
    login: &str,
    profiles: &[&str],
) -> i32 {
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

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_empty_login_returns_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    let v = auth::get_profiles(&state.db, "").await;
    assert_eq!(v, serde_json::json!([]), "no login -> []: {v}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_unknown_login_returns_default_only(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    let v = auth::get_profiles(&state.db, "nouser").await;
    assert_eq!(
        v,
        serde_json::json!([ { "label": "Default profile", "value": 0 } ]),
        "unknown login -> default only: {v}"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_lists_profiles_title_asc(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    // Inserted out of order; the response must be title-ascending.
    let uid = add_user_with_profiles(&state, "profuser", &["Zeta", "Alpha", "Mid"]).await;

    let v = auth::get_profiles(&state.db, "profuser").await;
    let arr = v.as_array().expect("array");
    assert_eq!(
        arr.len(),
        4,
        "default + 3 profiles: {v}"
    );
    assert_eq!(arr[0], serde_json::json!({ "label": "Default profile", "value": 0 }));
    let titles: Vec<&str> = arr[1..].iter().map(|p| p["label"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["Alpha", "Mid", "Zeta"], "title asc: {v}");
    // `value` carries the profile id (INT4).
    let alpha_id: i32 = sqlx::query_scalar(
        "SELECT id FROM ttrss_settings_profiles WHERE owner_uid = $1 AND title = 'Alpha'",
    )
    .bind(uid)
    .fetch_one(state.db.pool())
    .await
    .expect("alpha id");
    assert_eq!(arr[1]["value"], serde_json::json!(alpha_id));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_getprofiles_login_case_insensitive(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    add_user_with_profiles(&state, "CaseUser", &["P1"]).await;

    for login in ["caseuser", "CASEUSER", "CaseUser"] {
        let v = auth::get_profiles(&state.db, login).await;
        let arr = v.as_array().expect("array");
        assert_eq!(
            arr.len(),
            2,
            "case-insensitive match for {login:?}: {v}"
        );
    }
}
