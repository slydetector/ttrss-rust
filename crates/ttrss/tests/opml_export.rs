//! DB tests for the OPML **export** (M4: feed CRUD, OPML section).
//!
//! Rust-only: the frozen PHP suite has no `OPML` export tests, so these
//! exercise the ported `opml::build_opml` / `opml::export` directly on the
//! versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4 in categories
//! 1-2, labels 1-2, for owner 1. The bar is *functionally equivalent* OPML
//! (same feeds/categories/settings), not a byte-for-byte DOMDocument match.

mod common;

use ttrss::core::config::Config;
use ttrss::core::db::Db;
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

/// The feed-tree export (no settings): both categories, all four feeds, each
/// nested under its category, with `htmlUrl`, and *no* `ttrss*` attributes.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_export_feed_tree(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    let opml = ttrss::opml::build_opml(state.db.pool(), 1, false).await;

    // Envelope.
    assert!(
        opml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
        "xml decl: {opml}"
    );
    assert!(opml.contains("<opml version=\"1.0\">"));
    assert!(opml.contains("<title>Tiny Tiny RSS Feed Export</title>"));

    // Both categories, at depth 2 (4-space indent), `&` escaped.
    assert!(
        opml.contains("    <outline text=\"Technology\">"),
        "Technology cat: {opml}"
    );
    assert!(
        opml.contains("    <outline text=\"Science &amp; Nature\">"),
        "Science cat: {opml}"
    );

    // All four feeds, nested at depth 3 (6-space indent) with xmlUrl + htmlUrl.
    assert!(opml.contains("      <outline type=\"rss\" text=\"Hacker News\" xmlUrl=\"https://hnrss.org/frontpage\" htmlUrl=\"https://news.ycombinator.com\"/>"), "HN feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"The Register\" xmlUrl=\"https://www.theregister.com/headlines.atom\" htmlUrl=\"https://www.theregister.com\"/>"), "Register feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"Ars Technica\" xmlUrl=\"https://feeds.arstechnica.com/arstechnica/index\" htmlUrl=\"https://arstechnica.com\"/>"), "Ars feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"MIT Tech Review\" xmlUrl=\"https://www.technologyreview.com/feed/\" htmlUrl=\"https://www.technologyreview.com\"/>"), "MIT feed: {opml}");

    // No settings → no ttrss* attributes.
    assert!(!opml.contains("ttrssSortOrder"), "no ttrss attrs: {opml}");
    assert!(!opml.contains("tt-rss-prefs"));
}

/// With `include_settings`, the feed outlines carry the `ttrss*` attributes and
/// the category carries `ttrssSortOrder`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_export_include_settings(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    let opml = ttrss::opml::build_opml(state.db.pool(), 1, true).await;

    // Category sort order.
    assert!(
        opml.contains("    <outline text=\"Technology\" ttrssSortOrder=\"0\">"),
        "cat sort: {opml}"
    );

    // Feed ttrss* attrs (Hacker News has update_interval=30, order_id=0, purge=0).
    assert!(
        opml.contains("      <outline type=\"rss\" text=\"Hacker News\" xmlUrl=\"https://hnrss.org/frontpage\" ttrssSortOrder=\"0\" ttrssPurgeInterval=\"0\" ttrssUpdateInterval=\"30\" htmlUrl=\"https://news.ycombinator.com\"/>"),
        "HN ttrss: {opml}"
    );
    // MIT Tech Review has update_interval=120.
    assert!(
        opml.contains("ttrssUpdateInterval=\"120\""),
        "MIT ttrss: {opml}"
    );
}

/// The `export` handler returns the OPML as a download (content-type + filename).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_export_handler(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    let mut data = ttrss::core::session::Session::new();
    data.set("uid", ttrss::core::phpser::Value::Int(1));
    data.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);

    let extra = serde_json::Map::new();
    let (status, ct, disp, body) = ttrss::opml::export(&state, &sess, &extra).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(ct, "application/xml+opml");
    let disp = disp.expect("content-disposition");
    assert!(
        disp.starts_with("attachment; filename=tt-rss_admin_"),
        "disposition: {disp}"
    );
    assert!(disp.ends_with(".opml"), "disposition: {disp}");
    assert!(body.contains("<opml version=\"1.0\">"), "body: {body}");
    assert!(body.contains("text=\"Hacker News\""), "body: {body}");
}

/// Empty categories are RETAINED. PHP `opml_export_category` emits every
/// category that has a title (feeds or not); the "cleanup empty categories"
/// pass it runs afterwards is a no-op, because it queries `//outline[@title]`
/// while every exported outline uses `text` (not `title`). A functionally
/// equivalent export must reproduce this so an empty category round-trips on
/// import.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_export_retains_empty_category(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = make_state(pool, options).await;
    // Add an empty category (no feeds, no sub-cats).
    sqlx::query("INSERT INTO ttrss_feed_categories (owner_uid, title) VALUES (1, 'Empty Cat')")
        .execute(state.db.pool())
        .await
        .expect("insert empty cat");

    let opml = ttrss::opml::build_opml(state.db.pool(), 1, false).await;
    // The empty category is emitted as a self-closing outline (no children).
    assert!(
        opml.contains("    <outline text=\"Empty Cat\"/>"),
        "empty category retained: {opml}"
    );
    // The real categories remain.
    assert!(opml.contains("text=\"Technology\""));
    assert!(opml.contains("text=\"Science &amp; Nature\""));
}
