//! Test parity: M7.6 — the `no_iframes` plugin (iframes removed during
//! `HOOK_SANITIZE` unless `HOOK_IFRAME_WHITELISTED` says otherwise).
//!
//! The PHP `no_iframes` plugin has no dedicated suite in the frozen reference
//! (the M7 "no per-plugin PHP suites" note); the cases here pin the M7.6
//! verify bar: the full async `sanitize` pipeline (core + the `HOOK_SANITIZE`
//! stage) removes iframes when the plugin is active and is a no-op when it
//! is not, including the no-`src` iframe (PHP: no host → not whitelisted).
//! The whitelist chain itself is pinned by the unit tests (a custom
//! whitelisting plugin cannot live in `AppState`'s fixed registry; M7.11's
//! `af_youtube_embed` is the real consumer).

mod common;

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::session::Session;
use ttrss::core::phpser::Value as PValue;
use ttrss::pluginhost::host::HostCtx;
use ttrss::sanitizer;
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
        plugins: "no_iframes".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        max_cache_file_size: 64 * 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_max_jobs: 2,
        daemon_unsuccessful_days_limit: 30,
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_expire_lock_files: String::from("@hourly"),
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@hourly"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

async fn setup(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState::new(test_config(), db)
}

fn session() -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(100));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("no-iframe-test".into());
    sess.replace("no-iframe-test".into(), data);
    sess
}

/// The full async `sanitize` (core + the M7.6 `HOOK_SANITIZE` hook stage) with the
/// given active sets — the full pipeline the read paths use.
///
/// `has_sandbox` is `true`: with a sandbox-capable client the CORE keeps
/// (and sandboxes) iframes — `iframe` only enters the allowed-elements set
/// then — so removing them is exclusively the plugin's job (with
/// `has_sandbox = false` the core strips them and the plugin is moot).
async fn pipeline(
    state: &AppState,
    system: &[&str],
    user: &[&str],
    input: &str,
) -> String {
    let mut c =
        HostCtx::for_user(&state.plugins, state.db.pool(), Some(1), None, 0, system, user);
    sanitizer::sanitize(
        input,
        false,
        false,
        true,
        false,
        Some("http://site.example/"),
        None,
        42,
        Some(&mut c),
    )
    .await
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_pipeline_removes_iframes(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options).await;
    let _sess = session();

    // no_iframes active (system list).
    let out = pipeline(
        &state,
        &["no_iframes"],
        &[],
        "<p>a</p><iframe src=\"https://evil.example/x\" width=\"100\"></iframe><p>b</p><iframe></iframe><p>c</p>",
    )
    .await;
    // Both iframes go: the one with a src (host not whitelisted — no
    // whitelisting plugin registered) and the one without a src (PHP: no
    // host → not whitelisted). Everything else survives the core pass.
    assert!(!out.contains("<iframe"), "iframes survived: {out}");
    assert!(out.contains("a"), "{out}");
    assert!(out.contains("b"), "{out}");
    assert!(out.contains("c"), "{out}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_pipeline_noop_when_inactive(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options).await;
    let _sess = session();

    // Registered (it is in the registry) but not active for this owner:
    // the hook stage has an empty order and the iframe survives the core
    // pass — sandboxed, as `process_iframes` leaves it.
    let out = pipeline(&state, &[], &[], "<p>a</p><iframe src=\"https://evil.example/x\"></iframe><p>b</p>").await;
    assert!(out.contains("<iframe"), "iframe was removed without the plugin: {out}");
    assert!(out.contains("allow-scripts"), "core sandbox missing: {out}");
    assert!(out.contains("a"), "{out}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_user_enabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options).await;
    let _sess = session();

    // no_iframes is not a system plugin: the user-enabled list activates it
    // (the `_ENABLED_PLUGINS` shape the dispatch ctxs use).
    let out = pipeline(
        &state,
        &[],
        &["no_iframes"],
        "<p>a</p><iframe src=\"https://evil.example/x\"></iframe>",
    )
    .await;
    assert!(!out.contains("<iframe"), "iframes survived: {out}");
}
