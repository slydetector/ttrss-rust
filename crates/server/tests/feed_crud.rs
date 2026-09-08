//! DB tests for the `Pref_Feeds` write surface (M4: feed CRUD) — `editSave` /
//! `batchEditSave` (both `editsaveops`), `remove` (per-id `remove_feed`), and
//! the category add/rename/remove trio.
//!
//! Rust-only: the frozen PHP suite has no `Pref_Feeds` configuration tests, so
//! there is no 1:1 parity mapping — these exercise the ported handlers directly
//! on the versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4
//! (categories 1-2), labels 1-2, entries 1-11 for owner 1.

mod common;

use serde_json::{json, Map, Value};
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

/// A test-only config (the handlers only need `db` + `feed_icon_dir`).
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
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of all fixture rows).
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
    sqlx::query_scalar("SELECT count(*) FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = $1")
        .bind(title)
        .fetch_one(db.pool())
        .await
        .expect("cat count")
}

/// The `ref_id` set for owner 1, ordered.
async fn ref_ids(db: &Db) -> Vec<i32> {
    let rows: Vec<i32> =
        sqlx::query_scalar("SELECT ref_id FROM ttrss_user_entries WHERE owner_uid = 1 ORDER BY ref_id")
            .fetch_all(db.pool())
            .await
            .expect("ref ids");
    rows
}


/// `editSave` updates a single feed's columns (owner-scoped).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_edit_save_single(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let result = ttrss_server::feeds::edit_save(&state, &sess, &request(&[
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
    ]))
    .await;
    assert!(result.is_null(), "editSave returns no content, got {result}");

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
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_edit_save_cat_id_null(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Feed 1 is in category 1; clearing it sets `cat_id` to NULL.
    let _ = ttrss_server::feeds::edit_save(&state, &sess, &request(&[("id", json!("1")), ("cat_id", json!("0"))])).await;

    let f = feed_row(db, 1).await.expect("feed 1");
    assert_eq!(f.cat_id, None);
}

/// `editSave` on a feed the owner does not own is a no-op.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_edit_save_nonexistent(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // id 999 does not exist; no error, no row created.
    let _ = ttrss_server::feeds::edit_save(&state, &sess, &request(&[
        ("id", json!("999")),
        ("title", json!("Ghost")),
    ]))
    .await;
    assert!(feed_row(db, 999).await.is_none());
    // Feed 1 unchanged.
    assert_eq!(feed_row(db, 1).await.expect("feed 1").title, "Hacker News");
}

/// `batchEditSave` updates the given columns for each feed in `ids`, leaving
/// the rest of the owner's feeds untouched.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_batch_edit_save(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss_server::feeds::batch_edit_save(&state, &sess, &request(&[
        ("ids", json!([1, 2, 3])),
        ("update_interval", json!("66")),
        ("purge_interval", json!("2")),
        ("private", json!("on")),
        ("hide_images", json!("on")),
    ]))
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
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_batch_edit_save_empty_ids(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss_server::feeds::batch_edit_save(&state, &sess, &request(&[
        ("ids", json!([])),
        ("update_interval", json!("66")),
    ]))
    .await;

    for id in [1i32, 2, 3, 4] {
        let before_update = [30i32, 60, 60, 120][(id - 1) as usize];
        assert_eq!(feed_row(db, id).await.expect("feed").update_interval, before_update);
    }
}

/// `batchEditSave` updates `cat_id` only when `ENABLE_FEED_CATS` is on (the
/// default); with it off the category is left untouched.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_batch_edit_save_cat_respects_pref(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Default `ENABLE_FEED_CATS` is on: the category moves to 2.
    let _ = ttrss_server::feeds::batch_edit_save(&state, &sess, &request(&[
        ("ids", json!([1])),
        ("cat_id", json!("2")),
    ]))
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
    let _ = ttrss_server::feeds::batch_edit_save(&state, &sess, &request(&[
        ("ids", json!([1])),
        ("cat_id", json!("1")),
    ]))
    .await;
    assert_eq!(feed_row(db, 1).await.expect("feed 1").cat_id, Some(2), "cat_id should be untouched when the pref is off");
}

/// `batchEditSave` stores the auth credentials; with no encryption key the
/// password is written verbatim and `auth_pass_encrypted` is cleared.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_batch_edit_save_auth_pass(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss_server::feeds::batch_edit_save(&state, &sess, &request(&[
        ("ids", json!([1])),
        ("auth_login", json!("user1")),
        ("auth_pass", json!("s3cret")),
        ("include_in_digest", json!("on")),
        ("always_display_enclosures", json!("on")),
    ]))
    .await;

    let f = feed_row(db, 1).await.expect("feed 1");
    assert_eq!(f.id, 1);
    assert_eq!(f.auth_login, "user1");
    assert_eq!(f.auth_pass, "s3cret", "no encryption key → verbatim");
    assert!(!f.auth_pass_encrypted, "batch save clears the encrypted flag");
    assert!(f.include_in_digest);
    assert!(f.always_display_enclosures);
}

/// `remove` (positive id): the feed row is deleted; the starred entries are
/// preserved with `feed_id = NULL` (archived); the non-starred entries cascade
/// away; the feed's access keys are removed.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_remove_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Seed an access key for feed 3 (removed alongside the feed).
    sqlx::query("INSERT INTO ttrss_access_keys (access_key, feed_id, owner_uid) VALUES ('k-abc', '3', 1)")
        .execute(db.pool())
        .await
        .expect("access key");

    let _ = ttrss_server::feeds::remove_feeds(&state, &sess, &request(&[("ids", json!([3]))])).await;

    // The feed row is gone.
    assert!(feed_row(db, 3).await.is_none());
    // The access key is gone.
    let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM ttrss_access_keys WHERE feed_id = '3'")
        .fetch_one(db.pool())
        .await
        .expect("keys");
    assert_eq!(keys, 0);

    // Ref 7 (not starred) cascaded away; ref 8 (starred) survived, feedless.
    let ids = ref_ids(db).await;
    assert!(!ids.contains(&7), "ref 7 should be cascade-deleted");
    assert!(ids.contains(&8), "ref 8 (starred) should survive");
    let feed_id_8: Option<i32> = sqlx::query_scalar("SELECT feed_id FROM ttrss_user_entries WHERE ref_id = 8")
        .fetch_one(db.pool())
        .await
        .expect("ref 8 feed_id");
    assert_eq!(feed_id_8, None, "ref 8 feed_id should be NULL");
    // The other feeds' entries are untouched.
    assert!(ids.contains(&1) && ids.contains(&11));
}

/// `remove` (negative id) removes the label (and its assignments + cache).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
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
    let _ = ttrss_server::feeds::remove_feeds(&state, &sess, &request(&[("ids", json!([-1026]))])).await;

    let labels: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_labels2 WHERE id = 1").fetch_one(db.pool()).await.expect("labels");
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
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_add_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let _ = ttrss_server::feeds::add_cat(&state, &sess, &request(&[("cat", json!("News"))])).await;
    assert_eq!(cat_count(db, "News").await, 1);

    // Duplicate title → not created again.
    let _ = ttrss_server::feeds::add_cat(&state, &sess, &request(&[("cat", json!("News"))])).await;
    assert_eq!(cat_count(db, "News").await, 1);

    // The existing categories are unaffected.
    assert_eq!(cat_count(db, "Technology").await, 1);
}

/// `renameCat` renames an existing category; a missing id or empty title is a
/// no-op.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_rename_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Rename category 1 (Technology).
    let _ = ttrss_server::feeds::rename_cat(&state, &sess, &request(&[
        ("id", json!("1")),
        ("title", json!("Tech")),
    ]))
    .await;
    assert_eq!(cat_count(db, "Tech").await, 1);
    assert_eq!(cat_count(db, "Technology").await, 0);

    // Empty title → no-op (rename back then try empty).
    let _ = ttrss_server::feeds::rename_cat(&state, &sess, &request(&[
        ("id", json!("1")),
        ("title", json!("")),
    ]))
    .await;
    assert_eq!(cat_count(db, "Tech").await, 1, "empty title should not rename");

    // Non-existent id → no-op.
    let _ = ttrss_server::feeds::rename_cat(&state, &sess, &request(&[
        ("id", json!("99")),
        ("title", json!("Nope")),
    ]))
    .await;
    assert_eq!(cat_count(db, "Nope").await, 0);
}

/// `removeCat` deletes the category; the feeds keep their rows but their
/// `cat_id` is nulled (`ON DELETE SET NULL`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_remove_cat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Category 1 contains feeds 1, 2, 3.
    let _ = ttrss_server::feeds::remove_cat(&state, &sess, &request(&[("ids", json!([1]))])).await;

    assert_eq!(cat_count(db, "Technology").await, 0);
    // The feeds survive, now uncategorized.
    for id in [1i32, 2, 3] {
        let f = feed_row(db, id).await.expect("feed");
        assert_eq!(f.cat_id, None, "feed {id} cat_id should be NULL");
    }
    // Category 2 (feed 4) is untouched.
    assert_eq!(feed_row(db, 4).await.expect("feed 4").cat_id, Some(2));
}
