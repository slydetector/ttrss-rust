//! DB tests for `API::getCategories` (M5a.2: the `enable_nested` param).
//!
//! The tt-rss-android client sends `enable_nested=true` to get **top-level
//! categories only**, with every descendant's unread folded into each root
//! (`RootCategoriesModel.java:95`). PHP then runs the category query with
//! `where_null('parent_cat')` and adds `Feeds::_get_cat_children_unread` to
//! each root's (direct-feeds-only) counter.
//!
//! Fixture baseline (owner 1): cat 1 "Technology" (feeds 1-3, 6 direct
//! unread), cat 2 "Science & Nature" (feed 4, 2 unread). The tests add a
//! nested cat 3 "Hardware" under cat 1 with one unread entry.

mod common;

use serde_json::{json, Map, Value};
use ttrss::api;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
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

/// Fixtures + a session carrying `uid = 1` (owner of all fixture rows) + the
/// per-user API gate enabled (the `before_and_dispatch` auth check).
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(state.db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
    (state, sess)
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// The nested fixture the cases build on: cat 3 "Hardware" under cat 1, feed
/// 5 in cat 3, one unread entry on feed 5.
async fn add_nested_cat(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (id, owner_uid, parent_cat, title)
         VALUES (3, 1, 1, 'Hardware')",
    )
    .execute(pool)
    .await
    .expect("subcat insert");
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Hardware feed', 3, 'https://example.com/hw', 60, 0, 'https://example.com')",
    )
    .execute(pool)
    .await
    .expect("feed insert");
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
         VALUES (9, 'uuid-hw-001', 5, 1, true, false, '', '')",
    )
    .execute(pool)
    .await
    .expect("entry insert");
}

/// The real-category entries (id >= 1) as `id → unread`, dropping the special
/// cats (covered by the corpus).
fn real_cats(content: &Value) -> std::collections::BTreeMap<i64, i64> {
    let mut m = std::collections::BTreeMap::new();
    for e in content.as_array().expect("array response") {
        let id = e.get("id").and_then(Value::as_i64).expect("id");
        if id >= 1 {
            m.insert(
                id,
                e.get("unread").and_then(Value::as_i64).expect("unread"),
            );
        }
    }
    m
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_flat_lists_nested_cat_and_omits_descendant_unread(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    add_nested_cat(state.db.pool()).await;

    // No `enable_nested`: every category (roots and nested) is its own entry,
    // and each counter covers only its direct feeds.
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getcategories", &request(&[])).await;
    assert_eq!(status, 0);
    let cats = real_cats(&content);
    assert_eq!(cats.get(&1), Some(&6), "cat 1 direct unread; got {cats:?}");
    assert_eq!(cats.get(&2), Some(&2), "cat 2 direct unread; got {cats:?}");
    assert_eq!(cats.get(&3), Some(&1), "nested cat 3 listed; got {cats:?}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_enable_nested_lists_roots_only_and_folds_descendants(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    add_nested_cat(state.db.pool()).await;

    // `enable_nested` (the client's param): roots only, descendants' unread
    // folded in (cat 1 = 6 direct + 1 in nested cat 3).
    let (status, content) = api::before_and_dispatch(
        &state,
        &sess,
        "getcategories",
        &request(&[("enable_nested", json!("true"))]),
    )
    .await;
    assert_eq!(status, 0);
    let cats = real_cats(&content);
    assert_eq!(cats.get(&1), Some(&7), "cat 1 incl. descendants; got {cats:?}");
    assert_eq!(cats.get(&2), Some(&2), "cat 2 unchanged; got {cats:?}");
    assert!(!cats.contains_key(&3), "nested cat 3 must be absent; got {cats:?}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_enable_nested_unread_only_drops_zero_unread_roots(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    add_nested_cat(state.db.pool()).await;
    // Read all of cat 2's feed so the root has zero unread.
    sqlx::query("UPDATE ttrss_user_entries SET unread = false WHERE feed_id = 4")
        .execute(state.db.pool())
        .await
        .expect("mark read");

    let (status, content) = api::before_and_dispatch(
        &state,
        &sess,
        "getcategories",
        &request(&[
            ("enable_nested", json!("true")),
            ("unread_only", json!("true")),
        ]),
    )
    .await;
    assert_eq!(status, 0);
    let cats = real_cats(&content);
    assert_eq!(cats.get(&1), Some(&7), "cat 1 kept; got {cats:?}");
    assert!(!cats.contains_key(&2), "cat 2 (zero unread) dropped; got {cats:?}");
}

/// Without `include_empty`, the special cats are gated by `_is_cat_empty`,
/// which is `true` only for LABELS (no labels) / UNCATEGORIZED (no feeds).
/// SPECIAL is *never* empty (the PHP returns `false` for it), and its unread
/// is always 0. The fixture has labels, so LABELS is present; there are no
/// uncategorized feeds, so UNCATEGORIZED (id 0) is absent.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_special_cats_without_include_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getcategories", &request(&[])).await;
    assert_eq!(status, 0);

    let ids: Vec<i64> = content
        .as_array()
        .expect("array response")
        .iter()
        .map(|e| e.get("id").and_then(Value::as_i64).expect("id"))
        .collect();
    assert!(ids.contains(&-2), "LABELS present (labels exist): {ids:?}");
    assert!(ids.contains(&-1), "SPECIAL never empty: {ids:?}");
    assert!(!ids.contains(&0), "UNCATEGORIZED absent (no feeds): {ids:?}");

    // SPECIAL's unread is always 0 (PHP `_get_cat_unread` short-circuits it).
    let special = content
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.get("id").and_then(Value::as_i64) == Some(-1))
        .expect("SPECIAL entry");
    assert_eq!(special.get("unread").and_then(Value::as_i64), Some(0));
}
