//! DB tests for the `Pref_Feeds` write surface (M4: feed CRUD) — `editSave` /
//! `batchEditSave` (both `editsaveops`), `remove` (per-id `remove_feed`), and
//! the category add/rename/remove trio.
//!
//! Rust-only: the frozen PHP suite has no `Pref_Feeds` configuration tests, so
//! there is no 1:1 parity mapping — these exercise the ported handlers directly
//! on the versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4
//! (categories 1-2), labels 1-2, entries 1-11 for owner 1.

mod common;

use axum::body::to_bytes;
use axum::extract::{Extension, State};
use axum::http::{header, Method};
use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;

/// A test-only config (the handlers only need `db` + `feed_icon_dir`).
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

/// Fresh fixtures + a session carrying `uid = 1` (owner of all fixture rows).
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
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// One feed row: the columns `editsaveops` reads/writes.
struct Feed {
    id: i32,
    title: String,
    cat_id: Option<i32>,
    feed_url: String,
    site_url: String,
    update_interval: i32,
    purge_interval: i32,
    auth_login: String,
    private: bool,
    include_in_digest: bool,
    cache_images: bool,
    hide_images: bool,
    always_display_enclosures: bool,
    mark_unread_on_update: bool,
    feed_language: String,
    auth_pass: String,
    auth_pass_encrypted: bool,
}

async fn feed_row(db: &Db, id: i32) -> Option<Feed> {
    let row = sqlx::query(
        "SELECT id, title, cat_id, feed_url, site_url, update_interval, purge_interval,
                auth_login, private, include_in_digest, cache_images, hide_images,
                always_display_enclosures, mark_unread_on_update, feed_language,
                auth_pass, auth_pass_encrypted
         FROM ttrss_feeds WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .expect("feed row");
    row.map(|f| {
        use sqlx::Row as _;
        Feed {
            id: f.get(0),
            title: f.get(1),
            cat_id: f.get(2),
            feed_url: f.get(3),
            site_url: f.get(4),
            update_interval: f.get(5),
            purge_interval: f.get(6),
            auth_login: f.get(7),
            private: f.get(8),
            include_in_digest: f.get(9),
            cache_images: f.get(10),
            hide_images: f.get(11),
            always_display_enclosures: f.get(12),
            mark_unread_on_update: f.get(13),
            feed_language: f.get(14),
            auth_pass: f.get(15),
            auth_pass_encrypted: f.get(16),
        }
    })
}

async fn cat_count(db: &Db, title: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = $1",
    )
    .bind(title)
    .fetch_one(db.pool())
    .await
    .expect("cat count")
}

/// The `ref_id` set for owner 1, ordered.
async fn ref_ids(db: &Db) -> Vec<i32> {
    let rows: Vec<i32> = sqlx::query_scalar(
        "SELECT ref_id FROM ttrss_user_entries WHERE owner_uid = 1 ORDER BY ref_id",
    )
    .fetch_all(db.pool())
    .await
    .expect("ref ids");
    rows
}

/// `editSave` updates a single feed's columns (owner-scoped).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_edit_save_single(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let result = ttrss::feeds::edit_save(
        &state,
        &sess,
        &request(&[
            ("id", json!("1")),
            ("title", json!("HN Renamed")),
            ("feed_url", json!("https://hnrss.org/frontpage")),
            ("site_url", json!("https://news.ycombinator.com/")),
            ("update_interval", json!("45")),
            ("purge_interval", json!("7")),
            ("auth_login", json!("user1")),
            ("private", json!("on")),
            ("cache_images", json!("on")),
            ("feed_language", json!("en")),
            ("cat_id", json!("2")),
        ]),
    )
    .await;
    assert!(
        result.is_null(),
        "editSave returns no content, got {result}"
    );

    let f = feed_row(db, 1).await.expect("feed 1");
    assert_eq!(f.title, "HN Renamed");
    assert_eq!(f.site_url, "https://news.ycombinator.com/");
    assert_eq!(f.update_interval, 45);
    assert_eq!(f.purge_interval, 7);
    assert_eq!(f.auth_login, "user1");
    assert!(f.private);
    assert!(f.cache_images);
    assert_eq!(f.feed_language, "en");
    assert_eq!(f.cat_id, Some(2));
    // Untouched columns keep their seed values.
    assert!(!f.hide_images);
    assert!(!f.mark_unread_on_update);
    assert_eq!(f.feed_url, "https://hnrss.org/frontpage");
}

/// `editSave` with `cat_id = 0` clears the category (`NULL`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_edit_save_cat_id_null(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Feed 1 is in category 1; clearing it sets `cat_id` to NULL.
    let _ = ttrss::feeds::edit_save(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("cat_id", json!("0"))]),
    )
    .await;

    let f = feed_row(db, 1).await.expect("feed 1");
    assert_eq!(f.cat_id, None);
}

/// `editSave` on a feed the owner does not own is a no-op.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_edit_save_nonexistent(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // id 999 does not exist; no error, no row created.
    let _ = ttrss::feeds::edit_save(
        &state,
        &sess,
        &request(&[("id", json!("999")), ("title", json!("Ghost"))]),
    )
    .await;
    assert!(feed_row(db, 999).await.is_none());
    // Feed 1 unchanged.
    assert_eq!(feed_row(db, 1).await.expect("feed 1").title, "Hacker News");
}

/// `batchEditSave` updates the given columns for each feed in `ids`, leaving
/// the rest of the owner's feeds untouched.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_edit_save(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss::feeds::batch_edit_save(
        &state,
        &sess,
        &request(&[
            ("ids", json!([1, 2, 3])),
            ("update_interval", json!("66")),
            ("purge_interval", json!("2")),
            ("private", json!("on")),
            ("hide_images", json!("on")),
        ]),
    )
    .await;

    for id in [1i32, 2, 3] {
        let f = feed_row(db, id).await.expect("feed");
        assert_eq!(f.update_interval, 66, "feed {id}");
        assert_eq!(f.purge_interval, 2, "feed {id}");
        assert!(f.private, "feed {id}");
        assert!(f.hide_images, "feed {id}");
    }
    // Feed 4 is not in `ids` — untouched.
    let f4 = feed_row(db, 4).await.expect("feed 4");
    assert_eq!(f4.update_interval, 120);
    assert!(!f4.private);
    assert!(!f4.hide_images);
}

/// `batchEditSave` with an empty `ids` is a no-op.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_edit_save_empty_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss::feeds::batch_edit_save(
        &state,
        &sess,
        &request(&[("ids", json!([])), ("update_interval", json!("66"))]),
    )
    .await;

    for id in [1i32, 2, 3, 4] {
        let before_update = [30i32, 60, 60, 120][(id - 1) as usize];
        assert_eq!(
            feed_row(db, id).await.expect("feed").update_interval,
            before_update
        );
    }
}

/// `batchEditSave` updates `cat_id` only when `ENABLE_FEED_CATS` is on (the
/// default); with it off the category is left untouched.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_edit_save_cat_respects_pref(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Default `ENABLE_FEED_CATS` is on: the category moves to 2.
    let _ = ttrss::feeds::batch_edit_save(
        &state,
        &sess,
        &request(&[("ids", json!([1])), ("cat_id", json!("2"))]),
    )
    .await;
    assert_eq!(feed_row(db, 1).await.expect("feed 1").cat_id, Some(2));

    // Turn the pref off, then a `cat_id` batch update is a no-op.
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, 'ENABLE_FEED_CATS', NULL, '0')",
    )
    .execute(db.pool())
    .await
    .expect("set pref");
    let _ = ttrss::feeds::batch_edit_save(
        &state,
        &sess,
        &request(&[("ids", json!([1])), ("cat_id", json!("1"))]),
    )
    .await;
    assert_eq!(
        feed_row(db, 1).await.expect("feed 1").cat_id,
        Some(2),
        "cat_id should be untouched when the pref is off"
    );
}

/// `batchEditSave` stores the auth credentials; with no encryption key the
/// password is written verbatim and `auth_pass_encrypted` is cleared.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_edit_save_auth_pass(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss::feeds::batch_edit_save(
        &state,
        &sess,
        &request(&[
            ("ids", json!([1])),
            ("auth_login", json!("user1")),
            ("auth_pass", json!("s3cret")),
            ("include_in_digest", json!("on")),
            ("always_display_enclosures", json!("on")),
        ]),
    )
    .await;

    let f = feed_row(db, 1).await.expect("feed 1");
    assert_eq!(f.id, 1);
    assert_eq!(f.auth_login, "user1");
    assert_eq!(f.auth_pass, "s3cret", "no encryption key → verbatim");
    assert!(
        !f.auth_pass_encrypted,
        "batch save clears the encrypted flag"
    );
    assert!(f.include_in_digest);
    assert!(f.always_display_enclosures);
}

/// `remove` (positive id): the feed row is deleted; the starred entries are
/// preserved with `feed_id = NULL` (archived); the non-starred entries cascade
/// away; the feed's access keys are removed.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_remove_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Seed an access key for feed 3 (removed alongside the feed).
    sqlx::query(
        "INSERT INTO ttrss_access_keys (access_key, feed_id, owner_uid) VALUES ('k-abc', '3', 1)",
    )
    .execute(db.pool())
    .await
    .expect("access key");

    let _ = ttrss::feeds::remove_feeds(&state, &sess, &request(&[("ids", json!([3]))])).await;

    // The feed row is gone.
    assert!(feed_row(db, 3).await.is_none());
    // The access key is gone.
    let keys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_access_keys WHERE feed_id = '3'")
            .fetch_one(db.pool())
            .await
            .expect("keys");
    assert_eq!(keys, 0);

    // Ref 7 (not starred) cascaded away; ref 8 (starred) survived, feedless.
    let ids = ref_ids(db).await;
    assert!(!ids.contains(&7), "ref 7 should be cascade-deleted");
    assert!(ids.contains(&8), "ref 8 (starred) should survive");
    let feed_id_8: Option<i32> =
        sqlx::query_scalar("SELECT feed_id FROM ttrss_user_entries WHERE ref_id = 8")
            .fetch_one(db.pool())
            .await
            .expect("ref 8 feed_id");
    assert_eq!(feed_id_8, None, "ref 8 feed_id should be NULL");
    // The other feeds' entries are untouched.
    assert!(ids.contains(&1) && ids.contains(&11));
}

/// `remove` (negative id) removes the label (and its assignments + cache).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_remove_label(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Assign label 1 to refs 4 and 7.
    sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 4), (1, 7)")
        .execute(db.pool())
        .await
        .expect("label assignments");

    // feed id = `label_to_feed_id(1)` = -1024 - 1 - 1 = -1026.
    let _ = ttrss::feeds::remove_feeds(&state, &sess, &request(&[("ids", json!([-1026]))])).await;

    let labels: i64 = sqlx::query_scalar("SELECT count(*) FROM ttrss_labels2 WHERE id = 1")
        .fetch_one(db.pool())
        .await
        .expect("labels");
    assert_eq!(labels, 0, "label 1 should be removed");
    let assigned: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_user_labels2 WHERE label_id = 1")
            .fetch_one(db.pool())
            .await
            .expect("assignments");
    assert_eq!(assigned, 0, "label 1 assignments should be gone");
}

/// `addCat` creates a category; a second call with the same title is a no-op.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_add_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss::feeds::add_cat(&state, &sess, &request(&[("cat", json!("News"))])).await;
    assert_eq!(cat_count(db, "News").await, 1);

    // Duplicate title → not created again.
    let _ = ttrss::feeds::add_cat(&state, &sess, &request(&[("cat", json!("News"))])).await;
    assert_eq!(cat_count(db, "News").await, 1);

    // The existing categories are unaffected.
    assert_eq!(cat_count(db, "Technology").await, 1);
}

/// `renameCat` renames an existing category; a missing id or empty title is a
/// no-op.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_rename_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Rename category 1 (Technology).
    let _ = ttrss::feeds::rename_cat(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("title", json!("Tech"))]),
    )
    .await;
    assert_eq!(cat_count(db, "Tech").await, 1);
    assert_eq!(cat_count(db, "Technology").await, 0);

    // Empty title → no-op (rename back then try empty).
    let _ = ttrss::feeds::rename_cat(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("title", json!(""))]),
    )
    .await;
    assert_eq!(
        cat_count(db, "Tech").await,
        1,
        "empty title should not rename"
    );

    // Non-existent id → no-op.
    let _ = ttrss::feeds::rename_cat(
        &state,
        &sess,
        &request(&[("id", json!("99")), ("title", json!("Nope"))]),
    )
    .await;
    assert_eq!(cat_count(db, "Nope").await, 0);
}

/// `removeCat` deletes the category; the feeds keep their rows but their
/// `cat_id` is nulled (`ON DELETE SET NULL`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_remove_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Category 1 contains feeds 1, 2, 3.
    let _ = ttrss::feeds::remove_cat(&state, &sess, &request(&[("ids", json!([1]))])).await;

    assert_eq!(cat_count(db, "Technology").await, 0);
    // The feeds survive, now uncategorized.
    for id in [1i32, 2, 3] {
        let f = feed_row(db, id).await.expect("feed");
        assert_eq!(f.cat_id, None, "feed {id} cat_id should be NULL");
    }
    // Category 2 (feed 4) is untouched.
    assert_eq!(feed_row(db, 4).await.expect("feed 4").cat_id, Some(2));
}

// ============================================================================
// Ordering + access keys + icons (M4: feed CRUD, second increment)
// ============================================================================

/// `(order_id, cat_id)` for a feed.
async fn feed_order_cat(db: &Db, id: i32) -> (i32, Option<i32>) {
    let r = sqlx::query("SELECT order_id, cat_id FROM ttrss_feeds WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .expect("feed");
    use sqlx::Row as _;
    (r.get(0), r.get(1))
}

/// `(order_id, parent_cat)` for a category.
async fn cat_order_parent(db: &Db, id: i32) -> (i32, Option<i32>) {
    let r = sqlx::query("SELECT order_id, parent_cat FROM ttrss_feed_categories WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .expect("cat");
    use sqlx::Row as _;
    (r.get(0), r.get(1))
}

async fn access_key_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ttrss_access_keys WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .expect("count")
}

async fn access_key_for(db: &Db, feed_id: &str, is_cat: bool) -> Option<String> {
    sqlx::query_scalar(
        "SELECT access_key FROM ttrss_access_keys \
         WHERE owner_uid = 1 AND feed_id = $1 AND is_cat = $2",
    )
    .bind(feed_id)
    .bind(is_cat)
    .fetch_optional(db.pool())
    .await
    .expect("key")
}

/// `catsortreset` zeroes every category `order_id` for the owner.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_catsortreset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    sqlx::query("UPDATE ttrss_feed_categories SET order_id = 5 WHERE id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE ttrss_feed_categories SET order_id = 7 WHERE id = 2")
        .execute(db.pool())
        .await
        .unwrap();

    ttrss::feeds::catsortreset(&state, &sess, &request(&[])).await;

    assert_eq!(cat_order_parent(db, 1).await.0, 0);
    assert_eq!(cat_order_parent(db, 2).await.0, 0);
}

/// `feedsortreset` zeroes every feed `order_id` for the owner.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_feedsortreset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    sqlx::query("UPDATE ttrss_feeds SET order_id = 9 WHERE id = 1")
        .execute(db.pool())
        .await
        .unwrap();

    ttrss::feeds::feedsortreset(&state, &sess, &request(&[])).await;

    for id in [1i32, 2, 3, 4] {
        assert_eq!(feed_order_cat(db, id).await.0, 0);
    }
}

/// `savefeedorder` reorders feeds within a category and categories at the top
/// level (the dojo `payload` tree).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_savefeedorder_reorder(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let payload = json!({
        "items": [
            { "id": "root", "items": [
                { "_reference": "CAT:1" }, { "_reference": "CAT:2" } ] },
            { "id": "CAT:1", "items": [
                { "_reference": "FEED:2" }, { "_reference": "FEED:1" }, { "_reference": "FEED:3" } ] },
            { "id": "CAT:2", "items": [ { "_reference": "FEED:4" } ] }
        ]
    });
    let _ = ttrss::feeds::savefeedorder(
        &state,
        &sess,
        &request(&[("payload", json!(payload.to_string()))]),
    )
    .await;

    // Categories: top-level order + (top-level => parent NULL).
    assert_eq!(cat_order_parent(db, 1).await, (1, None));
    assert_eq!(cat_order_parent(db, 2).await, (2, None));
    // Feeds reordered inside CAT:1 (2,1,3) with cat_id kept.
    assert_eq!(feed_order_cat(db, 2).await, (1, Some(1)));
    assert_eq!(feed_order_cat(db, 1).await, (2, Some(1)));
    assert_eq!(feed_order_cat(db, 3).await, (3, Some(1)));
    // Feed 4 stays the sole (order 1) member of CAT:2.
    assert_eq!(feed_order_cat(db, 4).await, (1, Some(2)));
}

/// `savefeedorder` can move a feed to a different category (its `cat_id`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_savefeedorder_move(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let payload = json!({
        "items": [
            { "id": "root", "items": [
                { "_reference": "CAT:1" }, { "_reference": "CAT:2" } ] },
            { "id": "CAT:1", "items": [
                { "_reference": "FEED:1" }, { "_reference": "FEED:2" },
                { "_reference": "FEED:3" }, { "_reference": "FEED:4" } ] },
            { "id": "CAT:2", "items": [] }
        ]
    });
    let _ = ttrss::feeds::savefeedorder(
        &state,
        &sess,
        &request(&[("payload", json!(payload.to_string()))]),
    )
    .await;

    // Feed 4 moved from CAT:2 into CAT:1 as the 4th entry.
    assert_eq!(feed_order_cat(db, 4).await, (4, Some(1)));
    assert_eq!(feed_order_cat(db, 3).await, (3, Some(1)));
    // CAT:2 is now empty (still order 2, parent NULL).
    assert_eq!(cat_order_parent(db, 2).await, (2, None));
}

/// `clearKeys` deletes every access key for the owner.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_clear_keys(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    sqlx::query(
        "INSERT INTO ttrss_access_keys (access_key, feed_id, is_cat, owner_uid) \
         VALUES ('aaa', '1', false, 1), ('bbb', '2', false, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(access_key_count(db).await, 2);

    ttrss::feeds::clear_keys(&state, &sess, &request(&[])).await;

    assert_eq!(access_key_count(db).await, 0);
}

/// `regenFeedKey` creates a key if absent, and re-rolls it on each call (while
/// keeping a single row per (feed, is_cat)).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_regen_feed_key(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let r1 = ttrss::feeds::regen_feed_key(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("is_cat", json!("0"))]),
    )
    .await;
    let key1 = r1["link"].as_str().expect("key1").to_string();
    assert!(!key1.is_empty());
    assert_eq!(access_key_for(db, "1", false).await, Some(key1.clone()));

    let r2 = ttrss::feeds::regen_feed_key(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("is_cat", json!("0"))]),
    )
    .await;
    let key2 = r2["link"].as_str().expect("key2").to_string();
    assert!(!key2.is_empty());
    assert_ne!(key1, key2, "regenerated key should differ");
    // Still exactly one key row for (feed 1, is_cat false).
    assert_eq!(access_key_count(db).await, 1);
}

/// `getSharedURL` builds the public RSS share URL for a feed (title + link).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_get_shared_url(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let r = ttrss::feeds::get_shared_url(
        &state,
        &sess,
        &request(&[("id", json!("1")), ("is_cat", json!("0"))]),
    )
    .await;
    assert_eq!(r["title"].as_str(), Some("Hacker News"));
    let link = r["link"].as_str().expect("link");
    assert!(
        link.starts_with("http://127.0.0.1:8280/tt-rss/public.php?op=rss&id=1&is_cat=0&q="),
        "unexpected link: {link}"
    );
    // A key was created and is present + non-empty in the query string.
    let key = link.split("key=").nth(1).expect("key in link");
    assert!(!key.is_empty());
    assert_eq!(access_key_for(db, "1", false).await, Some(key.to_string()));
}

/// `removeIcon` deletes the favicon cache file and resets the feed's
/// `favicon_*` columns (only when the cached file exists and is removable).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_remove_icon(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    // A real, writable icon dir for this test.
    let icon_dir = std::env::temp_dir().join(format!("ttrss-feedicon-{}", std::process::id()));
    std::fs::create_dir_all(&icon_dir).unwrap();
    let mut cfg = test_config();
    cfg.feed_icon_dir = icon_dir.clone();
    let state = AppState::new(cfg, state.db.clone());

    // Mark feed 1 as a custom favicon and drop a cache file for it.
    sqlx::query(
        "UPDATE ttrss_feeds SET favicon_is_custom = true, favicon_avg_color = '#123456', \
         favicon_last_checked = '2024-01-01 00:00:00' WHERE id = 1",
    )
    .execute(db.pool())
    .await
    .unwrap();
    std::fs::write(icon_dir.join("1"), b"fake-favicon").unwrap();

    let _ = ttrss::feeds::remove_icon(&state, &sess, &request(&[("feed_id", json!("1"))])).await;

    assert!(!icon_dir.join("1").exists(), "icon file should be removed");
    let row = sqlx::query(
        "SELECT favicon_avg_color, favicon_last_checked, favicon_is_custom FROM ttrss_feeds WHERE id = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    use sqlx::Row as _;
    assert_eq!(
        row.get::<Option<String>, _>(0),
        None,
        "avg color reset to NULL"
    );
    assert_eq!(
        row.get::<Option<bool>, _>(2),
        Some(false),
        "is_custom reset to false"
    );
    let _ = std::fs::remove_dir_all(&icon_dir);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_upload_icon(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    use sqlx::Row as _;
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let icon_dir = std::env::temp_dir().join(format!("ttrss-feedicon-up-{}", std::process::id()));
    std::fs::create_dir_all(&icon_dir).unwrap();
    let mut cfg = test_config();
    cfg.feed_icon_dir = icon_dir.clone();
    let state = AppState::new(cfg, state.db.clone());

    // Success: upload a small icon for feed 1.
    let res = ttrss::feeds::upload_icon(
        &state,
        &sess,
        &request(&[("feed_id", json!("1"))]),
        Some(b"PNGDATA"),
    )
    .await;
    assert_eq!(
        res.get("rc").and_then(|v| v.as_str()),
        Some("E_ICON_UPLOAD_SUCCESS"),
        "res: {res}"
    );
    assert!(icon_dir.join("1").is_file(), "icon file written");
    let row =
        sqlx::query("SELECT favicon_avg_color, favicon_is_custom FROM ttrss_feeds WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        row.get::<Option<String>, _>(0),
        None,
        "avg color reset to NULL"
    );
    assert!(row.get::<bool, _>(1), "is_custom set to true");
    let url = res.get("icon_url").and_then(|v| v.as_str()).unwrap_or("");

    assert!(
        url.contains("op=feed_icon&id=1") && url.contains("?ts="),
        "icon_url cache-buster: {url}"
    );

    // File too large: a 10-byte upload with a 4-byte cap.
    let mut cfg2 = test_config();
    cfg2.feed_icon_dir = icon_dir.clone();
    cfg2.max_favicon_file_size = 4;
    let state2 = AppState::new(cfg2, state.db.clone());
    let res2 = ttrss::feeds::upload_icon(
        &state2,
        &sess,
        &request(&[("feed_id", json!("2"))]),
        Some(&[0u8; 10]),
    )
    .await;
    assert_eq!(
        res2.get("rc").and_then(|v| v.as_str()),
        Some("E_ICON_FILE_TOO_LARGE"),
        "res2: {res2}"
    );
    assert!(
        !icon_dir.join("2").exists(),
        "no file written for too-large upload"
    );

    // No such feed.
    let res3 = ttrss::feeds::upload_icon(
        &state,
        &sess,
        &request(&[("feed_id", json!("999"))]),
        Some(b"PNGDATA"),
    )
    .await;
    assert_eq!(
        res3.get("rc").and_then(|v| v.as_str()),
        Some("E_ICON_UPLOAD_FAILED")
    );

    // No file part at all.
    let res4 =
        ttrss::feeds::upload_icon(&state, &sess, &request(&[("feed_id", json!("3"))]), None).await;
    assert_eq!(
        res4.get("rc").and_then(|v| v.as_str()),
        Some("E_ICON_UPLOAD_FAILED")
    );

    let _ = std::fs::remove_dir_all(&icon_dir);
}

// ---------------------------------------------------------------------------
// batchSubscribe / batchAddFeeds (the batch-add-feeds write surface)
// ---------------------------------------------------------------------------

/// `batchAddFeeds`: each line of the posted feed list becomes a `[Unknown]`
/// feed for the user (posted cat/login/pass, `update_method = 0`, pass stored
/// unencrypted).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_add_feeds(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    use sqlx::Row as _;
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    ttrss::feeds::batch_add_feeds(
        &state,
        &sess,
        &request(&[
            ("cat", json!(1)),
            (
                "feeds",
                json!(
                    "http://example.com/feed1\nhttp://example.com/feed2\nhttps://example.org/rss"
                ),
            ),
            ("login", json!("user")),
            ("pass", json!("secret")),
        ]),
    )
    .await;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1 AND title = '[Unknown]'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 3, "three [Unknown] feeds inserted");

    let row = sqlx::query(
        "SELECT title, cat_id, auth_login, auth_pass, update_method, auth_pass_encrypted \
         FROM ttrss_feeds WHERE feed_url = 'http://example.com/feed1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>(0), "[Unknown]");
    assert_eq!(row.get::<Option<i32>, _>(1), Some(1));
    assert_eq!(row.get::<Option<String>, _>(2), Some("user".to_string()));
    assert_eq!(row.get::<Option<String>, _>(3), Some("secret".to_string()));
    assert_eq!(row.get::<i32, _>(4), 0);
    assert!(!row.get::<bool, _>(5), "auth_pass is stored unencrypted");
}

/// `batchAddFeeds`: a feed URL the user already has is not re-inserted (only
/// the new one is added).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_add_feeds_skip_existing(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // `https://hnrss.org/frontpage` is feed 1 (exists); the other is new.
    ttrss::feeds::batch_add_feeds(
        &state,
        &sess,
        &request(&[(
            "feeds",
            json!("https://hnrss.org/frontpage\nhttp://example.com/new"),
        )]),
    )
    .await;
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        after,
        before + 1,
        "existing feed skipped, only the new one added"
    );
    let dup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE feed_url = 'https://hnrss.org/frontpage'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(dup, 1, "no duplicate of the existing feed");
}

/// `batchAddFeeds`: invalid / non-http lines are skipped by `UrlHelper::validate`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_add_feeds_invalid_skipped(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    ttrss::feeds::batch_add_feeds(
        &state,
        &sess,
        &request(&[(
            "feeds",
            json!("http://example.com/valid\nnot a url\nftp://example.com/feed"),
        )]),
    )
    .await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1 AND title = '[Unknown]'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1, "only the valid http feed is inserted");
}

/// `batchAddFeeds`: a read-only user is a no-op (no feeds added).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_add_feeds_readonly_noop(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    sess.with_mut(|s| s.set("access_level", PValue::Int(-1)));
    let db = &state.db;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    ttrss::feeds::batch_add_feeds(
        &state,
        &sess,
        &request(&[("feeds", json!("http://example.com/x"))]),
    )
    .await;
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(after, before, "read-only user adds nothing");
}

/// `batchSubscribe`: returns `enable_cats` as an **int** (`1`/`0`, per the PHP
/// `(int)` cast) + the category `<select>`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_batch_subscribe(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let v = ttrss::feeds::batch_subscribe(&state, &sess).await;
    // The PHP `(int)` cast — an int, not a JSON boolean.
    let enable = v.get("enable_cats").and_then(|x| x.as_i64());
    assert!(
        enable == Some(0) || enable == Some(1),
        "enable_cats is 0/1: {v}"
    );
    let cat_select = v.get("cat_select").and_then(|x| x.as_str()).unwrap_or("");
    assert!(
        cat_select.contains("<select"),
        "cat_select is a select: {cat_select}"
    );
    assert!(
        cat_select.contains("Uncategorized"),
        "uncategorized option: {cat_select}"
    );
}

/// The dispatcher matches methods **case-insensitively** (PHP `backend.php`
/// lowercases `$method`, so `call`/`method_exists` resolve case-insensitively).
/// Verified through the public `rpc::handler`: the frontend's lowercase
/// `inactivefeeds`/`batchaddfeeds`/`batchsubscribe`, a corpus-style camelCase
/// `markSelected`, and a mixed-case `getFeedTree` all route to a real handler
/// (not `E_UNKNOWN_METHOD`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_dispatch_case_insensitive(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db);

    // A session for `uid = 1` (admin) with the stored pwd hash + a valid csrf
    // token, so `validate_session` + the CSRF gate pass and routing is the test.
    fn sess() -> Sess {
        let mut d = Session::new();
        d.set("uid", PValue::Int(1));
        d.set("access_level", PValue::Int(10));
        d.set(
            "pwd_hash",
            PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
        );
        d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
        d.set("name", PValue::Bytes(b"admin".to_vec()));
        d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
        let s = Sess::new("test-sid".to_string());
        s.replace("test-sid".to_string(), d);
        s
    }

    fn req(body: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(Method::POST)
            .uri("/backend.php")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    async fn call(state: &AppState, body: &str) -> Value {
        let res = ttrss::rpc::handler(State(state.clone()), Extension(sess()), req(body)).await;
        let bytes = to_bytes(res.into_body(), 10 * 1024 * 1024)
            .await
            .expect("body");
        serde_json::from_str(&String::from_utf8_lossy(&bytes)).expect("json")
    }

    // `batchsubscribe` (lowercased from the frontend's `batchSubscribe`) returns
    // the dialog data — proving the route AND the handler ran (needs a valid
    // session).
    let v = call(&state, "op=Pref_Feeds&method=batchsubscribe&csrf_token=tok").await;
    assert!(
        v.get("enable_cats").is_some(),
        "batchsubscribe dialog data: {v}"
    );

    // These must route (not `E_UNKNOWN_METHOD`), across cases the frontend/corpus
    // actually send.
    for body in [
        "op=Pref_Feeds&method=inactivefeeds&csrf_token=tok",
        "op=Pref_Feeds&method=inactiveFeeds&csrf_token=tok",
        "op=Pref_Feeds&method=getFeedTree&csrf_token=tok",
        "op=Pref_Feeds&method=batchaddfeeds&csrf_token=tok&feeds=http://example.com/rt",
        "op=RPC&method=markSelected&csrf_token=tok",
    ] {
        let v = call(&state, body).await;
        assert_ne!(
            v["error"]["code"].as_str(),
            Some("E_UNKNOWN_METHOD"),
            "routed for {body:?}: {v}"
        );
    }
}
