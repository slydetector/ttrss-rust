//! DB tests for the `Pref_Prefs` auth write handlers (`changePersonalData`,
//! `changepassword`, `otpenable`, `otpdisable`) + the `customizeCSS` read.
//! Rust-only: the frozen PHP suite has no `Pref_Prefs` auth tests, so these
//! exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures (owner 1 = the session uid, whose baseline password is
//! `password`).
mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::core::user;
use ttrss::session::Sess;
use ttrss::AppState;

/// A test-only config (the handlers only need `db`).
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
        max_cache_file_size: 64 * 1024 * 1024,
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

/// Fresh fixtures + a session carrying `uid = 1`, the admin access level, the
/// `auth_internal` module and the `admin` login name.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// A `POST` form field map from (name, value) pairs.
fn form(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_change_personal_data(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    let extra = form(&[("full_name", "Test User"), ("email", "test@example.com")]);
    let out = ttrss::settings::change_personal_data(&state, &sess, &extra).await;
    assert_eq!(out, "Your personal data has been saved.");

    let row: (String, String) =
        sqlx::query_as("SELECT full_name, email FROM ttrss_users WHERE id = 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(row.0, "Test User");
    assert_eq!(row.1, "test@example.com");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_changepassword_success(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    let extra = form(&[
        ("old_password", "password"),
        ("new_password", "newpass"),
        ("confirm_password", "newpass"),
    ]);
    let out = ttrss::settings::changepassword(&state, &sess, &extra).await;
    assert!(out.contains("Password has been changed."), "got: {out}");
    assert!(
        out.contains("alert-info"),
        "success should be a notice, got: {out}"
    );

    // The stored hash now verifies the new password (with the fresh salt),
    // and the session's cached `pwd_hash` was refreshed.
    let (pwd_hash, salt): (String, String) =
        sqlx::query_as("SELECT pwd_hash, salt FROM ttrss_users WHERE id = 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert!(user::verify_password(&pwd_hash, "newpass", &salt));
    assert!(!user::verify_password(&pwd_hash, "password", &salt));
    assert_eq!(sess.data().pwd_hash(), Some(pwd_hash.as_str()));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_changepassword_validation_errors(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    // Each pair is (form fields, expected message substring). The baseline
    // password is irrelevant — validation short-circuits before verification.
    let cases: &[(&[(&str, &str)], &str)] = &[
        (
            &[
                ("old_password", "a"),
                ("new_password", "<b>x</b>"),
                ("confirm_password", "<b>x</b>"),
            ],
            "New password contains disallowed characters.",
        ),
        (
            &[
                ("old_password", "abc"),
                ("new_password", "abc"),
                ("confirm_password", "abc"),
            ],
            "New password must be different from the old one.",
        ),
        (
            &[
                ("old_password", ""),
                ("new_password", "xyz"),
                ("confirm_password", "xyz"),
            ],
            "Old password cannot be blank.",
        ),
        (
            &[
                ("old_password", "abc"),
                ("new_password", ""),
                ("confirm_password", ""),
            ],
            "New password cannot be blank.",
        ),
        (
            &[
                ("old_password", "abc"),
                ("new_password", "xyz"),
                ("confirm_password", "other"),
            ],
            "Entered passwords do not match.",
        ),
    ];
    for (fields, expect) in cases {
        let out = ttrss::settings::changepassword(&state, &sess, &form(fields)).await;
        assert!(out.contains(expect), "expected {expect:?} in: {out}");
        assert!(
            out.contains("alert-danger"),
            "error should be alert-danger, got: {out}"
        );
        assert!(
            out.starts_with("ERROR: "),
            "should be prefixed with ERROR:, got: {out}"
        );
    }
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_changepassword_wrong_old_password(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let extra = form(&[
        ("old_password", "not-the-password"),
        ("new_password", "newpass"),
        ("confirm_password", "newpass"),
    ]);
    let out = ttrss::settings::changepassword(&state, &sess, &extra).await;
    assert!(out.contains("Old password is incorrect."), "got: {out}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_changepassword_forbidden(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (mut state, sess) = setup(pool, options).await;
    state.config.forbid_password_changes = true;
    let extra = form(&[
        ("old_password", "password"),
        ("new_password", "newpass"),
        ("confirm_password", "newpass"),
    ]);
    let out = ttrss::settings::changepassword(&state, &sess, &extra).await;
    assert!(out.starts_with("ERROR: "), "got: {out}");
    assert!(out.contains("Access forbidden."), "got: {out}");
    assert!(out.contains("alert-danger"), "got: {out}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_otp_enable(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // A known secret so we can compute the current TOTP code.
    let known_secret = "0123456789abcdef0123456789abcdef";
    sqlx::query("UPDATE ttrss_users SET otp_secret = $1 WHERE id = 1")
        .bind(known_secret)
        .execute(db)
        .await
        .unwrap();
    let code = user::totp_at(known_secret.as_bytes(), ttrss::util::now_unix() as u64);

    // Wrong password.
    let out = ttrss::settings::otp_enable(
        &state,
        &sess,
        &form(&[("password", "wrong"), ("otp", &code)]),
    )
    .await;
    assert_eq!(out, "ERROR:Incorrect password");

    // Wrong OTP code.
    let out = ttrss::settings::otp_enable(
        &state,
        &sess,
        &form(&[("password", "password"), ("otp", "000000")]),
    )
    .await;
    assert_eq!(out, "ERROR:Incorrect one time password");

    // Correct password + code → enabled.
    let out = ttrss::settings::otp_enable(
        &state,
        &sess,
        &form(&[("password", "password"), ("otp", &code)]),
    )
    .await;
    assert_eq!(out, "OK");
    let enabled: bool = sqlx::query_scalar("SELECT otp_enabled FROM ttrss_users WHERE id = 1")
        .fetch_one(db)
        .await
        .unwrap();
    assert!(enabled);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_otp_disable(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // Wrong password first.
    let out = ttrss::settings::otp_disable(&state, &sess, &form(&[("password", "wrong")])).await;
    assert_eq!(out, "ERROR: Incorrect password");
    let still: bool = sqlx::query_scalar("SELECT otp_enabled FROM ttrss_users WHERE id = 1")
        .fetch_one(db)
        .await
        .unwrap();
    assert!(!still);

    // Enable, then disable with the correct password.
    sqlx::query("UPDATE ttrss_users SET otp_enabled = true, otp_secret = 'deadbeef' WHERE id = 1")
        .execute(db)
        .await
        .unwrap();
    let out = ttrss::settings::otp_disable(&state, &sess, &form(&[("password", "password")])).await;
    assert_eq!(out, "OK");
    let (enabled, secret): (bool, Option<String>) =
        sqlx::query_as("SELECT otp_enabled, otp_secret FROM ttrss_users WHERE id = 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert!(!enabled);
    assert!(
        secret.is_none(),
        "secret should be cleared on disable (schema >= 143)"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_customize_css(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, 'USER_STYLESHEET', NULL, $1)",
    )
    .bind("body { color: red; }<br/>/* second line */")
    .execute(db)
    .await
    .unwrap();

    let out = ttrss::settings::customize_css(&state, &sess).await;
    let expect = json!({ "value": "body { color: red; }\n/* second line */" });
    assert_eq!(out, expect, "got: {out}");
}
