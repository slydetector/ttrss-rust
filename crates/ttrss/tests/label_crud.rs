//! DB tests for the `Pref_Labels` label CRUD (M4): `edit`, `add`, `remove`,
//! `save` (rename, incl. the "Assign label" filter-reference update),
//! `colorset`, and `colorreset`.
//!
//! Rust-only: the frozen PHP suite has no `Pref_Labels` configuration tests, so
//! there is no 1:1 parity mapping — these exercise the ported handlers directly
//! on the versioned `schema` + `baseline` + `seed` fixtures: labels 1-2
//! (`test-label-1`, `test-label-2`) for owner 1.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
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

/// The label row (or `None`): `{id, owner_uid, fg_color, bg_color, caption}`.
async fn label_row(db: &Db, id: i32) -> Option<Map<String, Value>> {
    let row = sqlx::query(
        "SELECT id, owner_uid, fg_color, bg_color, caption FROM ttrss_labels2 WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .expect("label row");
    use sqlx::Row as _;
    row.map(|r| {
        Map::from_iter([
            ("id".to_string(), json!(r.get::<i32, _>(0))),
            ("owner_uid".to_string(), json!(r.get::<i32, _>(1))),
            ("fg_color".to_string(), json!(r.get::<String, _>(2))),
            ("bg_color".to_string(), json!(r.get::<String, _>(3))),
            ("caption".to_string(), json!(r.get::<String, _>(4))),
        ])
    })
}

/// The owner-1 label cache values (all `label_cache` columns), ordered.
async fn label_caches(db: &Db) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT label_cache FROM ttrss_user_entries WHERE owner_uid = 1 ORDER BY int_id",
    )
    .fetch_all(db.pool())
    .await
    .expect("label caches")
}

/// The count of "Assign label" filter actions (action_id = 7) with a given
/// param for the owner's filters.
async fn label_action_count(db: &Db, param: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2_actions
         WHERE action_id = 7 AND action_param = $1
           AND filter_id IN (SELECT id FROM ttrss_filters2 WHERE owner_uid = 1)",
    )
    .bind(param)
    .fetch_one(db.pool())
    .await
    .expect("label action count")
}

/// `Pref_Labels::edit`: returns the label row as a JSON object.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_edit(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let v = ttrss::labels::edit(&state, &sess, &request(&[("id", json!(1))])).await;
    let v = v.expect("label found");
    assert_eq!(v["caption"], "test-label-1");
    assert_eq!(v["id"], 1);
    assert_eq!(v["owner_uid"], 1);
    assert_eq!(v["fg_color"], "");
    assert_eq!(v["bg_color"], "");

    // Another owner's labels are not visible (owner_uid filter).
    let mut data = Session::new();
    data.set("uid", PValue::Int(999));
    let sess2 = Sess::new("other-sid".to_string());
    sess2.replace("other-sid".to_string(), data);
    assert!(
        ttrss::labels::edit(&state, &sess2, &request(&[("id", json!(1))]))
            .await
            .is_none()
    );
}

/// `Pref_Labels::edit`: a nonexistent id returns `None` (PHP prints nothing).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_edit_not_found(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    assert!(
        ttrss::labels::edit(&state, &sess, &request(&[("id", json!(9999))]))
            .await
            .is_none()
    );
}

/// `Pref_Labels::add`: creates the label and returns `Created label <b>..</b>`;
/// a duplicate caption is not created and returns empty.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_add_label(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let out = ttrss::labels::add_label(&state, &sess, &request(&[("caption", json!("My Label"))]))
        .await
        .expect("add");
    assert_eq!(out, "Created label <b>My Label</b>");
    assert_eq!(ttrss::labels::find_id(db.pool(), "My Label", 1).await, 3);

    // A duplicate caption is not created (case-insensitive) and returns empty.
    let out = ttrss::labels::add_label(&state, &sess, &request(&[("caption", json!("my label"))]))
        .await
        .expect("add dup");
    assert_eq!(out, "");
    assert_eq!(ttrss::labels::find_id(db.pool(), "my label", 1).await, 3);

    // An empty caption creates nothing.
    let out = ttrss::labels::add_label(&state, &sess, &request(&[("caption", json!(""))]))
        .await
        .expect("add empty");
    assert_eq!(out, "");
    assert_eq!(ttrss::labels::find_id(db.pool(), "", 1).await, 0);
}

/// `Pref_Labels::add`: `output` set suppresses the message (still creates).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_add_label_output_suppress(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let out = ttrss::labels::add_label(
        &state,
        &sess,
        &request(&[("caption", json!("Quiet Label")), ("output", json!("1"))]),
    )
    .await
    .expect("add");
    assert_eq!(out, "");
    assert_eq!(ttrss::labels::find_id(db.pool(), "Quiet Label", 1).await, 3);
}

/// `Pref_Labels::remove`: removes the label, its access key, and clears the
/// owner's label caches; other labels are unaffected.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_remove_labels(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let pool = db.pool();

    // An access key for label 1 (feed_id = label_to_feed_id(1) = -1026).
    sqlx::query("INSERT INTO ttrss_access_keys (access_key, feed_id, is_cat, owner_uid) VALUES ($1, '-1026', false, 1)")
        .bind("testkey")
        .execute(pool)
        .await
        .expect("insert key");
    // A non-empty label cache on an owner-1 entry (to verify it is cleared).
    let entries: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_user_entries WHERE owner_uid = 1")
            .fetch_one(pool)
            .await
            .expect("count");
    assert!(entries > 0, "expected owner-1 entries in the seed");
    sqlx::query("UPDATE ttrss_user_entries SET label_cache = '[1]' WHERE owner_uid = 1")
        .execute(pool)
        .await
        .expect("set cache");

    ttrss::labels::remove_labels(&state, &sess, &request(&[("ids", json!("1"))]))
        .await
        .expect("remove");

    // The label is gone.
    assert!(label_row(db, 1).await.is_none());
    // The access key is gone.
    let keys: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_access_keys WHERE feed_id = '-1026' AND owner_uid = 1",
    )
    .fetch_one(pool)
    .await
    .expect("keys");
    assert_eq!(keys, 0);
    // The label caches are cleared.
    for c in label_caches(db).await {
        assert_eq!(c, "");
    }
    // The other label is unaffected.
    assert!(label_row(db, 2).await.is_some());
}

/// `Pref_Labels::save`: renames the label and rewrites the "Assign label"
/// filter actions that referenced the old caption.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_save_rename(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let pool = db.pool();

    // A filter (owner 1) with an "Assign label" action referencing the caption.
    let filter_id: i32 =
        sqlx::query_scalar("INSERT INTO ttrss_filters2 (owner_uid) VALUES (1) RETURNING id")
            .fetch_one(pool)
            .await
            .expect("insert filter");
    sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, 7, 'test-label-1')")
        .bind(filter_id)
        .execute(pool)
        .await
        .expect("insert action");

    let out = ttrss::labels::save(
        &state,
        &sess,
        &request(&[("id", json!(1)), ("caption", json!("Renamed Label"))]),
    )
    .await
    .expect("save");
    assert_eq!(out, "Renamed Label");

    // The label is renamed.
    assert_eq!(
        label_row(db, 1).await.expect("label 1")["caption"],
        "Renamed Label"
    );
    // The filter action reference is rewritten (old caption gone, new present).
    assert_eq!(label_action_count(db, "test-label-1").await, 0);
    assert_eq!(label_action_count(db, "Renamed Label").await, 1);
}

/// `Pref_Labels::save`: renaming to a caption another of the owner's labels
/// already uses is a no-op (returns the old caption).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_save_collision(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    let out = ttrss::labels::save(
        &state,
        &sess,
        &request(&[("id", json!(1)), ("caption", json!("test-label-2"))]),
    )
    .await
    .expect("save");
    assert_eq!(out, "test-label-1");
    assert_eq!(
        label_row(db, 1).await.expect("label 1")["caption"],
        "test-label-1"
    );
}

/// `Pref_Labels::save`: a nonexistent label is a no-op (returns empty).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_save_not_found(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let out = ttrss::labels::save(
        &state,
        &sess,
        &request(&[("id", json!(9999)), ("caption", json!("X"))]),
    )
    .await
    .expect("save");
    assert_eq!(out, "");
}

/// `Pref_Labels::colorset`: `kind = "both"` sets both fg and bg colors and
/// clears the owner's label caches.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_colorset_both(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let pool = db.pool();

    // Set a label cache so we can verify colorset clears it.
    sqlx::query("UPDATE ttrss_user_entries SET label_cache = '[1]' WHERE owner_uid = 1")
        .execute(pool)
        .await
        .expect("set cache");

    ttrss::labels::colorset(
        &state,
        &sess,
        &request(&[
            ("kind", json!("both")),
            ("ids", json!(1)),
            ("fg", json!("#111111")),
            ("bg", json!("#222222")),
        ]),
    )
    .await
    .expect("colorset");

    let row = label_row(db, 1).await.expect("label 1");
    assert_eq!(row["fg_color"], "#111111");
    assert_eq!(row["bg_color"], "#222222");
    // The label caches are cleared.
    for c in label_caches(db).await {
        assert_eq!(c, "");
    }
    // The other label is unaffected.
    assert_eq!(label_row(db, 2).await.expect("label 2")["fg_color"], "");
}

/// `Pref_Labels::colorset`: `kind = "fg"` sets only the fg column (from
/// `color`), leaving bg unchanged.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_colorset_single(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    ttrss::labels::colorset(
        &state,
        &sess,
        &request(&[
            ("kind", json!("fg")),
            ("ids", json!(1)),
            ("color", json!("#abcdef")),
        ]),
    )
    .await
    .expect("colorset");

    let row = label_row(db, 1).await.expect("label 1");
    assert_eq!(row["fg_color"], "#abcdef");
    assert_eq!(row["bg_color"], "");
}

/// `Pref_Labels::colorreset`: resets the selected labels' colors to `''`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_colorreset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let pool = db.pool();

    sqlx::query("UPDATE ttrss_labels2 SET fg_color = '#111', bg_color = '#222' WHERE id = 1")
        .execute(pool)
        .await
        .expect("set colors");

    ttrss::labels::colorreset(&state, &sess, &request(&[("ids", json!(1))]))
        .await
        .expect("colorreset");

    let row = label_row(db, 1).await.expect("label 1");
    assert_eq!(row["fg_color"], "");
    assert_eq!(row["bg_color"], "");
    // The other label is unaffected.
    assert_eq!(label_row(db, 2).await.expect("label 2")["fg_color"], "");
}
