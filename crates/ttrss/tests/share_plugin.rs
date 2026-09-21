//! Test: M7.13 — the `share` plugin (share an article by a unique URL; the key
//! is a short uuid in the **`ttrss_user_entries.uuid`** column, keyed
//! `ref_id` + `owner_uid`).
//!
//! The PHP has no unit tests for this plugin; these are new behavior coverage
//! for the M7.13 verify bar — the key lifecycle (`newkey` / `unshare` /
//! `clearArticleKeys`), the `shareDialog` (reuses a stored key, mints one when
//! empty, "Article not found." with no row), and the owner scoping (a different
//! owner cannot touch or read the article's key). Driven through
//! `pluginhost::dispatch::pluginhandler` (the protected path) exactly as the
//! RPC layer would; the no-account public route (`get`) is covered separately.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::dispatch::{pluginhandler, public_dispatch};
use ttrss::pluginhost::plugin::MethodOutput;
use ttrss::session::Sess;
use ttrss::AppState;

fn test_config(plugins: &str) -> Config {
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
        plugins: plugins.into(),
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
        fetch_extended_url_filtering: false,
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

async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
    plugins: &str,
) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

/// A logged-in session for `uid` with CSRF token `tok` (as login would create).
fn session(uid: i32) -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(uid as i64));
    data.set("access_level", PValue::Int(100));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("share-test".into());
    sess.replace("share-test".into(), data);
    sess
}

/// The request map for a `share` plugin method (id-only methods).
fn req(id: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("plugin".into(), json!("share"));
    if let Some(i) = id {
        m.insert("id".into(), json!(i));
    }
    m
}

fn json_of(v: &MethodOutput) -> &Value {
    match v {
        MethodOutput::Json(j) => j,
        MethodOutput::Raw(s) => panic!("expected JSON, got raw: {s}"),
        MethodOutput::Response { .. } => panic!("expected JSON, got a response"),
    }
}

fn raw(v: &MethodOutput) -> &str {
    match v {
        MethodOutput::Raw(s) => s,
        MethodOutput::Response { body, .. } => body,
        MethodOutput::Json(j) => panic!("expected raw HTML, got JSON: {j}"),
    }
}

/// The article's stored uuid (the seed value or the plugin's key).
async fn stored_uuid(pool: &sqlx::PgPool, ref_id: i32, owner: i32) -> String {
    sqlx::query_scalar("SELECT uuid FROM ttrss_user_entries WHERE int_id = $1 AND owner_uid = $2")
        .bind(ref_id)
        .bind(owner)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or_default()
}

/// Count of the owner's articles that still carry a non-empty uuid.
async fn shared_count(pool: &sqlx::PgPool, owner: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE owner_uid = $1 AND uuid <> ''",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap()
}


// `newkey` mints a fresh uuid for the article (overwriting the seed's key) and
// echoes `{"link": …}`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_newkey_mints_key(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool.clone(), options, "share").await;
    let sess = session(1);
    let before = stored_uuid(&state.db.pool(), 1, 1).await;

    let v = pluginhandler(&state, &sess, "tok", "newkey", &req(Some("1"))).await.unwrap();
    let j = json_of(&v);
    let new_key: String = j["link"].as_str().unwrap().into();
    assert!(!new_key.is_empty());
    assert_ne!(new_key, before, "newkey should overwrite the seed key");

    assert_eq!(stored_uuid(&state.db.pool(), 1, 1).await, new_key);
}

// `unshare` clears the article's uuid + echoes the message.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_unshare_clears_key(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let sess = session(1);

    let v = pluginhandler(&state, &sess, "tok", "unshare", &req(Some("1"))).await.unwrap();
    assert_eq!(raw(&v), "Article unshared");
    assert_eq!(stored_uuid(&state.db.pool(), 1, 1).await, "");
}

// `clearArticleKeys` blanks every one of the owner's article keys.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_clear_article_keys(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let sess = session(1);
    assert!(shared_count(&state.db.pool(), 1).await > 0, "seed should pre-share");

    let v = pluginhandler(&state, &sess, "tok", "cleararticlekeys", &req(None)).await.unwrap();
    assert_eq!(raw(&v), "Shared URLs cleared.");
    assert_eq!(shared_count(&state.db.pool(), 1).await, 0);
}

// `shareDialog` reuses a stored key (the seed's `uuid-hn-001` for article 1)
// without minting a new one.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_share_dialog_reuses_key(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let sess = session(1);

    let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("1"))).await.unwrap();
    let html = raw(&v);
    // The seed key is used in the share URL…
    assert!(html.contains("key=uuid-hn-001"), "{html}");
    // …and the footer buttons are present.
    assert!(html.contains("Unshare article"), "{html}");
    assert!(html.contains("Generate new URL"), "{html}");
    assert!(html.contains("Close this window"), "{html}");
    // Reusing does not change the stored key.
    assert_eq!(stored_uuid(&state.db.pool(), 1, 1).await, "uuid-hn-001");
}

// `shareDialog` on an article whose key was cleared mints a fresh one.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_share_dialog_mints_when_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let sess = session(1);

    // Clear the seed key first.
    pluginhandler(&state, &sess, "tok", "unshare", &req(Some("1"))).await.unwrap();
    assert_eq!(stored_uuid(&state.db.pool(), 1, 1).await, "");

    let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("1"))).await.unwrap();
    let html = raw(&v);
    // A fresh key was minted (not the seed, not empty).
    let new_key = stored_uuid(&state.db.pool(), 1, 1).await;
    assert!(!new_key.is_empty() && new_key != "uuid-hn-001");
    assert!(html.contains(&format!("key={new_key}")), "{html}");
}

// `shareDialog` for an article with no `ttrss_user_entries` row (owner 1 has no
// article 9999) → "Article not found.", no key minted.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_share_dialog_no_row(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let sess = session(1);

    let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("9999"))).await.unwrap();
    let html = raw(&v);
    assert!(html.contains("Article not found."), "{html}");
}

// Owner scoping: owner 2 has no article 1, so neither its `unshare` clears
// owner 1's key nor its `shareDialog` can read it.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_owner_scoping(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;
    let owner2 = session(2);

    // owner 2's unshare on owner 1's article is a no-op (no row for owner 2).
    pluginhandler(&state, &owner2, "tok", "unshare", &req(Some("1"))).await.unwrap();
    assert_eq!(
        stored_uuid(&state.db.pool(), 1, 1).await,
        "uuid-hn-001",
        "owner 2 must not clear owner 1's key"
    );

    // …and owner 2's dialog cannot read owner 1's article.
    let v = pluginhandler(&state, &owner2, "tok", "sharedialog", &req(Some("1")))
        .await
        .unwrap();
    assert!(raw(&v).contains("Article not found."), "{}", raw(&v));
}

// `get` public route: a valid key serves the article page (200, text/html).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_get_public_route(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;

    // The seed pre-sets uuid-hn-001 on article 1 (owner 1).
    let v = public_dispatch(&state, "share", "get", &json!({"key": "uuid-hn-001"}))
        .await
        .unwrap();
    let MethodOutput::Response { status, content_type, body } = v else {
        panic!("expected a Response, got {v:?}");
    };
    assert_eq!(status, 200, "body: {body}");
    assert!(content_type.starts_with("text/html"), "content_type: {content_type}");
    assert!(body.contains("<!DOCTYPE html>"), "body: {body}");
}

// `get` public route: an unknown key → 404 "Article not found."
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_get_public_route_404(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "share").await;

    let v = public_dispatch(&state, "share", "get", &json!({"key": "nonexistent"}))
        .await
        .unwrap();
    let MethodOutput::Response { status, body, .. } = v else {
        panic!("expected a Response, got {v:?}");
    };
    assert_eq!(status, 404, "body: {body}");
    assert_eq!(body, "Article not found.");
}
