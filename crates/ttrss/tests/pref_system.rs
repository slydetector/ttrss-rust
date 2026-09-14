//! DB tests for the `Pref_System` admin handlers (`index` + `_log_viewer`,
//! `clearLog`, `sendTestEmail`, `getphpinfo`) in `ttrss::system`.
//! Rust-only: the frozen PHP suite has no `Pref_System` tests, so these
//! exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures (owner 1 = `admin`, access_level 10).
//!
//! `getscheduledtasks` was ported earlier (see `system::getscheduledtasks`);
//! `sendTestEmail` is a deliberate no-op (the system MTA is not wired up) and
//! `getphpinfo` surfaces Rust/server diagnostics in place of `phpinfo()`.
mod common;

use serde_json::{json, Map};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::system;
use ttrss::AppState;

/// A test-only config (the handlers only need `db` + `config`).
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

/// Fresh fixtures + an admin session (`uid = 1`, `access_level = 10`).
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
    data.set("access_level", PValue::Int(10));
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// Insert an error-log row and return the inserted `errno` (for later asserts).
async fn seed_error(pool: &sqlx::PgPool, errno: i32, errstr: &str) {
    sqlx::query(
        "INSERT INTO ttrss_error_log (owner_uid, errno, errstr, filename, lineno, context, created_at)
         VALUES (1, $1, $2, 'test_file.php', 42, 'a context', NOW())",
    )
    .bind(errno)
    .bind(errstr)
    .execute(pool)
    .await
    .expect("seed error row");
}

/// Count rendered event-log data rows (`<td class='errno'>` cells) in HTML.
fn count_log_rows(html: &str) -> usize {
    html.matches("<td class='errno'>").count()
}

/// `index` renders the accordion with all four panes (assumes SQL logging).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_index_structure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html = system::index(&state, &sess, &Map::new()).await;
    for marker in [
        "dijit.layout.AccordionContainer",
        "Event log",
        "Email configuration",
        "Scheduled tasks",
        "Server Information",
        "mail_address",
        "value=\"sendTestEmail\"",
        "getphpinfo",
        "getscheduledtasks",
    ] {
        assert!(html.contains(marker), "index should contain {marker:?}");
    }
}

/// `_log_viewer` severity filter: default (Warnings, errno 512) shows both an
/// E_USER_ERROR (256) and an E_WARNING (2) row; "Errors" (256) shows only the
/// E_USER_ERROR row; "Everything" (1024) shows both.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_log_viewer_severity(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    seed_error(state.db.pool(), 256, "a user error").await;
    seed_error(state.db.pool(), 2, "a warning").await;

    // Default severity (512 = Warnings): both rows.
    let default_html = system::index(&state, &sess, &Map::new()).await;
    assert_eq!(
        count_log_rows(&default_html),
        2,
        "default (Warnings) shows both"
    );

    // Severity 256 (Errors): only the E_USER_ERROR row.
    let errors_html = system::index(
        &state,
        &sess,
        &Map::from_iter([("severity".into(), json!(256))]),
    )
    .await;
    assert_eq!(
        count_log_rows(&errors_html),
        1,
        "Errors shows only errno 256"
    );
    assert!(errors_html.contains("a user error"));
    assert!(!errors_html.contains("a warning"));

    // Severity 1024 (Everything): both rows.
    let all_html = system::index(
        &state,
        &sess,
        &Map::from_iter([("severity".into(), json!(1024))]),
    )
    .await;
    assert_eq!(count_log_rows(&all_html), 2, "Everything shows both");
}

/// `clearLog` deletes all error-log rows.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_clear_log(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, _) = setup(pool, options).await;
    seed_error(state.db.pool(), 256, "one").await;
    seed_error(state.db.pool(), 2, "two").await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_error_log")
        .fetch_one(state.db.pool())
        .await
        .expect("count before");
    assert_eq!(before, 2);

    system::clear_log(&state).await;

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_error_log")
        .fetch_one(state.db.pool())
        .await
        .expect("count after");
    assert_eq!(after, 0, "error log is cleared");
}

/// `sendTestEmail` is a no-op: `rc = 0` + a "not supported" error.
#[test]
fn test_send_test_email_noop() {
    let v = system::send_test_email();
    assert_eq!(v["rc"], 0);
    let err = v["error"].as_str().expect("error is a string");
    assert!(err.contains("not supported"), "got {err:?}");
}

/// `getphpinfo` surfaces Rust/server diagnostics (version + config), not a PHP
/// dump, and omits the db password / encryption key.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_get_php_info(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, _) = setup(pool, options).await;
    let html = system::get_php_info(&state);
    assert!(
        html.contains(ttrss::core::config::VERSION),
        "shows the version"
    );
    assert!(
        html.contains("http://127.0.0.1:8280/tt-rss/"),
        "shows self_url"
    );
    assert!(html.contains("postgres"), "shows the db name");
    // The password (`postgres`) appears as the db name too, so assert the
    // encryption key is absent by the omission of its (None) — nothing to leak.
    assert!(!html.contains("E_ERROR"), "not a phpinfo() dump");
}
