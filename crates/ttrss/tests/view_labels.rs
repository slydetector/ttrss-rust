//! Rust-only regression test for the `Feeds::view` label-cache side effect.
//!
//! PHP's `_format_headlines_list` (Feeds.php) renders each article's labels
//! and, for an article with `num_labels > 0` but an empty `label_cache`, falls
//! back to `Article::_get_labels($id)` — the **hard path**, which ALSO writes
//! the result back to `ttrss_user_entries.label_cache` (`Labels::update_cache`).
//! That write is load-bearing: a later `Article::_labels_of` (in the mutation
//! handlers, used by the SPA's post-marking `getAllCounters` refresh) derives
//! the affected labels from `label_cache`, so the per-label counters update
//! immediately instead of waiting for the next poll.
//!
//! The port originally rendered the labels in the empty-cache branch *without*
//! that write (a local `get_labels_fallback`), so the SPA's label unread badge
//! stuck while scrolling. The fix routes the branch through the canonical
//! `labels::get_labels_json` (the faithful `_get_labels` port that writes the
//! cache).
//!
//! The frozen PHP suite only exercises `_get_headlines` (the query builder),
//! not this side effect, so this Rust-only test pins the corrected behavior:
//! after `view` renders a labeled, empty-cache article, its `label_cache` is
//! populated.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::headlines::view;
use ttrss::labels::label_to_feed_id;
use ttrss::session::Sess;
use ttrss::AppState;

const SESSION_UID: i64 = 1;

/// A test-only config (the handler only needs `db`).
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
        docroot: std::path::PathBuf::from("tt-rss"),
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
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(SESSION_UID));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// The `label_cache` text for one user entry, or `None` if the row is absent.
async fn label_cache(pool: &sqlx::Pool<sqlx::Postgres>, article_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT label_cache FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
    )
    .bind(article_id)
    .bind(SESSION_UID)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_view_populates_label_cache(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let p = state.db.pool();

    // Link label 1 to article 1 (in feed 1) so article 1 has `num_labels = 1`
    // while its `label_cache` is still empty (as the seed leaves it).
    sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 1)")
        .execute(p)
        .await
        .unwrap();

    // Precondition: the seed leaves the cache empty.
    assert_eq!(
        label_cache(p, 1).await.as_deref(),
        Some(""),
        "article 1 starts with an empty label_cache"
    );

    let expected_feed_id = label_to_feed_id(1);

    // Render feed 1 (which contains article 1) via the SPA `view` handler.
    let extra: Map<String, Value> = Map::from_iter(vec![("feed".to_string(), json!("1"))]);
    let reply = view(&state, &sess, &extra).await;

    // The article's labels are present in the response…
    let labels = reply
        .get("headlines")
        .and_then(|h| h.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arts| arts.iter().find(|a| a.get("id") == Some(&json!(1))))
        .and_then(|a| a.get("labels"))
        .and_then(|l| l.as_array())
        .expect("article 1 in the view has a labels array");
    assert!(
        labels.iter().any(|l| l[0] == json!(expected_feed_id)),
        "labels array contains the virtual feed id for label 1: {labels:?}"
    );

    // …AND the hard path wrote the cache back (the `_get_labels` side effect).
    let cached = label_cache(p, 1).await.unwrap_or_default();
    assert!(
        !cached.is_empty(),
        "label_cache is populated after view: {cached:?}"
    );
    assert!(
        cached.contains(&expected_feed_id.to_string()),
        "cache records label 1's virtual feed id: {cached:?}"
    );
}
