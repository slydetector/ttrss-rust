//! DB tests for the `RPC` config verbs (M4): `setpref`, `togglepref`,
//! `setWidescreen`, and `log` (the client-JS error reporter).
//!
//! Rust-only: the frozen PHP suite has no tests for these small handlers, so
//! these exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures. Owner 1 (the admin user in `schema.sql`) is the session uid.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;

type Pool = sqlx::PgPool;

/// A test-only config (the handlers only need `db`).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
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

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState {
        config: test_config(),
        db,
    };
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// The default-profile (profile IS NULL) override value for a pref (None if
/// the override row is absent).
async fn pref_value(db: &Pool, pref_name: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 \
         WHERE owner_uid = 1 AND pref_name = $1 AND profile IS NULL",
    )
    .bind(pref_name)
    .fetch_optional(db)
    .await
    .unwrap()
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_set_pref_sets_bool(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // `COMBINED_DISPLAY_MODE` default is "true"; posting "false" stores "" and
    // echoes the posted value back.
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[
            ("key", json!("COMBINED_DISPLAY_MODE")),
            ("value", json!("false")),
        ]),
    )
    .await;
    assert_eq!(
        pref_value(db, "COMBINED_DISPLAY_MODE").await.as_deref(),
        Some("")
    );
    assert_eq!(
        rv,
        json!({ "param": "COMBINED_DISPLAY_MODE", "value": "false" })
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_set_pref_stylesheet_keeps_tags(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // `USER_STYLESHEET`: `strip_tags` is off, so a CSS value with `<...>` is
    // stored verbatim (not mangled).
    let posted = "<div>body { }</div>";
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[("key", json!("USER_STYLESHEET")), ("value", json!(posted))]),
    )
    .await;
    assert_eq!(
        pref_value(db, "USER_STYLESHEET").await.as_deref(),
        Some(posted)
    );
    assert_eq!(rv, json!({ "param": "USER_STYLESHEET", "value": posted }));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_set_pref_strips_tags_other(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // A non-stylesheet Str pref: `strip_tags` is on, so the stored value is
    // stripped while the response echoes the *posted* value.
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[
            ("key", json!("SHORT_DATE_FORMAT")),
            ("value", json!("<b>M d</b>")),
        ]),
    )
    .await;
    assert_eq!(
        pref_value(db, "SHORT_DATE_FORMAT").await.as_deref(),
        Some("M d")
    );
    assert_eq!(
        rv,
        json!({ "param": "SHORT_DATE_FORMAT", "value": "<b>M d</b>" })
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_toggle_pref_flips_bool(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // `HIDE_READ_FEEDS` default is "false"; toggling stores "1" and echoes the
    // resolved typed value (a JSON bool for a T_BOOL pref).
    let rv1 = ttrss::settings::toggle_pref(
        &state,
        &sess,
        &request(&[("key", json!("HIDE_READ_FEEDS"))]),
    )
    .await;
    assert_eq!(
        pref_value(db, "HIDE_READ_FEEDS").await.as_deref(),
        Some("1")
    );
    assert_eq!(rv1, json!({ "param": "HIDE_READ_FEEDS", "value": true }));

    // Toggling again flips it back to false.
    let rv2 = ttrss::settings::toggle_pref(
        &state,
        &sess,
        &request(&[("key", json!("HIDE_READ_FEEDS"))]),
    )
    .await;
    assert_eq!(pref_value(db, "HIDE_READ_FEEDS").await.as_deref(), Some(""));
    assert_eq!(rv2, json!({ "param": "HIDE_READ_FEEDS", "value": false }));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_set_widescreen(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    let rv1 =
        ttrss::settings::set_widescreen(&state, &sess, &request(&[("wide", json!("1"))])).await;
    assert_eq!(
        pref_value(db, "WIDESCREEN_MODE").await.as_deref(),
        Some("1")
    );
    assert_eq!(rv1, json!({ "wide": 1 }));

    let rv2 =
        ttrss::settings::set_widescreen(&state, &sess, &request(&[("wide", json!("0"))])).await;
    assert_eq!(pref_value(db, "WIDESCREEN_MODE").await.as_deref(), Some(""));
    assert_eq!(rv2, json!({ "wide": 0 }));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_client_log_acks(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let rv = ttrss::settings::client_log(
        &state,
        &sess,
        &request(&[
            ("msg", json!("some client error")),
            ("file", json!("js/foo.js")),
            ("line", json!("42")),
            ("context", json!("ctx")),
        ]),
    )
    .await;
    assert_eq!(rv, json!({ "message": "HOST_ERROR_LOGGED" }));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_client_log_empty_noop(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    // An empty `msg` logs nothing and echoes nothing (JSON `null`).
    let rv = ttrss::settings::client_log(&state, &sess, &request(&[("msg", json!(""))])).await;
    assert_eq!(rv, Value::Null);
}
