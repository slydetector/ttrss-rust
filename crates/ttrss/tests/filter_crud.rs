//! DB tests for the `Pref_Filters` filter CRUD (M4): `editSave`, `add`,
//! `remove`, `clone`, `join`, `savefilterorder`, and `filtersortreset`.
//!
//! Rust-only: the frozen PHP suite has no `Pref_Filters` configuration tests,
//! so there is no 1:1 parity mapping — these exercise the ported handlers
//! directly on the versioned `schema` + `baseline` fixtures (no filter rows are
//! seeded; the reference `ttrss_filter_types` / `ttrss_filter_actions` tables
//! live in `schema.sql`). Owner 1 is the test session's uid.

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

/// Fresh fixtures + a session carrying `uid = 1`.
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

/// Wrap a JSON object into the array-of-JSON-strings shape the front-end posts
/// for `rule`/`action` params.
fn rule_param(rules: &[Value]) -> Value {
    json!(rules.iter().map(|r| r.to_string()).collect::<Vec<_>>())
}

/// Insert a bare filter row and return its id.
async fn insert_filter(db: &Db, title: &str) -> i32 {
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, title, enabled) VALUES (1, $1, true) RETURNING id",
    )
    .bind(title)
    .fetch_one(db.pool())
    .await
    .expect("insert filter");
    id
}

/// The filter row (or `None`).
async fn filter_row(db: &Db, id: i32) -> Option<Map<String, Value>> {
    let row = sqlx::query(
        "SELECT id, title, enabled, match_any_rule, inverse, order_id FROM ttrss_filters2 WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .expect("filter row");
    use sqlx::Row as _;
    row.map(|r| {
        Map::from_iter([
            ("id".to_string(), json!(r.get::<i32, _>(0))),
            ("title".to_string(), json!(r.get::<String, _>(1))),
            ("enabled".to_string(), json!(r.get::<bool, _>(2))),
            ("match_any_rule".to_string(), json!(r.get::<bool, _>(3))),
            ("inverse".to_string(), json!(r.get::<bool, _>(4))),
            ("order_id".to_string(), json!(r.get::<i32, _>(5))),
        ])
    })
}

/// Count of a filter's rules.
async fn rule_count(db: &Db, filter_id: i32) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ttrss_filters2_rules WHERE filter_id = $1")
        .bind(filter_id)
        .fetch_one(db.pool())
        .await
        .expect("rule count")
}

/// A filter's rules (ordered by id): `{reg_exp, filter_type, inverse}`.
async fn rules(db: &Db, filter_id: i32) -> Vec<Map<String, Value>> {
    let rows = sqlx::query(
        "SELECT reg_exp, filter_type, inverse FROM ttrss_filters2_rules WHERE filter_id = $1 ORDER BY id",
    )
    .bind(filter_id)
    .fetch_all(db.pool())
    .await
    .expect("rules");
    use sqlx::Row as _;
    rows.iter()
        .map(|r| {
            Map::from_iter([
                ("reg_exp".to_string(), json!(r.get::<String, _>(0))),
                ("filter_type".to_string(), json!(r.get::<i32, _>(1))),
                ("inverse".to_string(), json!(r.get::<bool, _>(2))),
            ])
        })
        .collect()
}

/// A filter's actions (ordered by id): `{action_id, action_param}`.
async fn actions(db: &Db, filter_id: i32) -> Vec<Map<String, Value>> {
    let rows = sqlx::query(
        "SELECT action_id, action_param FROM ttrss_filters2_actions WHERE filter_id = $1 ORDER BY id",
    )
    .bind(filter_id)
    .fetch_all(db.pool())
    .await
    .expect("actions");
    use sqlx::Row as _;
    rows.iter()
        .map(|r| {
            Map::from_iter([
                ("action_id".to_string(), json!(r.get::<i32, _>(0))),
                ("action_param".to_string(), json!(r.get::<String, _>(1))),
            ])
        })
        .collect()
}

/// Count of all filters for the owner.
async fn filter_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ttrss_filters2 WHERE owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .expect("filter count")
}

// ---------------------------------------------------------------------------
// `filtersortreset`
// ---------------------------------------------------------------------------

/// `Pref_Filters::filtersortreset` — every filter's order is reset to 0.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_filtersortreset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let id = insert_filter(db, "F1").await;
    sqlx::query("UPDATE ttrss_filters2 SET order_id = 5 WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();

    ttrss::filters::filtersortreset(&state, &sess)
        .await
        .expect("reset");
    let row = filter_row(db, id).await.expect("filter");
    assert_eq!(row["order_id"], 0);
}

// ---------------------------------------------------------------------------
// `savefilterorder`
// ---------------------------------------------------------------------------

/// `Pref_Filters::savefilterorder` — applies the reordered filter list from
/// the `payload` JSON.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_savefilterorder(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let a = insert_filter(db, "A").await;
    let b = insert_filter(db, "B").await;
    let c = insert_filter(db, "C").await;

    // Reorder: B, C, A (and an out-of-owner entry that must be ignored).
    let payload = json!({
        "items": [{
            "items": [
                { "_reference": format!("FILTER:{b}") },
                { "_reference": format!("FILTER:{c}") },
                { "_reference": format!("FILTER:{a}") },
            ]
        }]
    });
    ttrss::filters::savefilterorder(
        &state,
        &sess,
        &request(&[("payload", json!(payload.to_string()))]),
    )
    .await
    .expect("save order");

    assert_eq!(filter_row(db, b).await.unwrap()["order_id"], 0);
    assert_eq!(filter_row(db, c).await.unwrap()["order_id"], 1);
    assert_eq!(filter_row(db, a).await.unwrap()["order_id"], 2);
}

// ---------------------------------------------------------------------------
// `remove`
// ---------------------------------------------------------------------------

/// `Pref_Filters::remove` — deletes the selected owned filters; rules and
/// actions cascade. A foreign owner's filter is untouched.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_remove_filters(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let id = insert_filter(db, "F1").await;
    sqlx::query(
        "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, 'x', 1)",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, 1, '')").bind(id).execute(db.pool()).await.unwrap();

    ttrss::filters::remove_filters(&state, &sess, &request(&[("ids", json!(id.to_string()))]))
        .await
        .expect("remove");

    assert!(filter_row(db, id).await.is_none());
    assert_eq!(rule_count(db, id).await, 0);
    assert_eq!(actions(db, id).await.len(), 0);
    assert_eq!(filter_count(db).await, 0);

    // An empty `ids` list is a no-op.
    let keep = insert_filter(db, "Keep").await;
    ttrss::filters::remove_filters(&state, &sess, &request(&[]))
        .await
        .expect("noop");
    assert!(filter_row(db, keep).await.is_some());
}

// ---------------------------------------------------------------------------
// `editSave`
// ---------------------------------------------------------------------------

/// `Pref_Filters::editSave` — updates the owned filter row (title is trimmed,
/// not tag-stripped) and re-saves its rules/actions. A foreign owner's filter
/// is untouched.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_edit_save(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let id = insert_filter(db, "  Old Title ").await;
    // A pre-existing rule that must be replaced.
    sqlx::query(
        "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, 'old', 1)",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();

    let rule_payload =
        rule_param(&[json!({"reg_exp": "  new  ", "filter_type": 1, "inverse": true})]);
    let req = request(&[
        ("id", json!(id.to_string())),
        ("title", json!(" New <b>Title</b> ")),
        ("enabled", json!("off")),
        ("match_any_rule", json!("on")),
        ("inverse", json!("on")),
        ("rule", rule_payload),
    ]);
    ttrss::filters::edit_save(&state, &sess, &req)
        .await
        .expect("save");

    let row = filter_row(db, id).await.expect("filter");
    // trim (not clean): whitespace trimmed, tags preserved.
    assert_eq!(row["title"], "New <b>Title</b>");
    assert_eq!(row["enabled"], false);
    assert_eq!(row["match_any_rule"], true);
    assert_eq!(row["inverse"], true);
    // The rule was replaced (trim of reg_exp, inverse set).
    let rs = rules(db, id).await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["reg_exp"], "new");
    assert_eq!(rs[0]["inverse"], true);

    // A foreign owner's filter is not modified.
    let other_uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash) VALUES ('other', 'x') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let other: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, title) VALUES ($1, 'Other') RETURNING id",
    )
    .bind(other_uid)
    .fetch_one(db.pool())
    .await
    .unwrap();
    ttrss::filters::edit_save(
        &state,
        &sess,
        &request(&[("id", json!(other.to_string())), ("title", json!("Hacked"))]),
    )
    .await
    .expect("save other");
    assert_eq!(filter_row(db, other).await.unwrap()["title"], "Other");
}

// ---------------------------------------------------------------------------
// `add`
// ---------------------------------------------------------------------------

/// `Pref_Filters::add` — creates a filter (default enabled from the checkbox)
/// with the posted rules.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_add_filter(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let before = filter_count(db).await;

    let req = request(&[
        ("title", json!("My Filter")),
        ("enabled", json!("on")),
        ("match_any_rule", json!("on")),
        ("inverse", json!("")),
        (
            "rule",
            rule_param(&[json!({"reg_exp": "hello", "filter_type": 2})]),
        ),
        (
            "action",
            rule_param(&[json!({"action_id": 2, "action_param": ""})]),
        ),
    ]);
    ttrss::filters::add_filter(&state, &sess, &req)
        .await
        .expect("add");

    assert_eq!(filter_count(db).await, before + 1);
    let id: i32 = sqlx::query_scalar("SELECT id FROM ttrss_filters2 WHERE title = 'My Filter'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let row = filter_row(db, id).await.unwrap();
    assert_eq!(row["enabled"], true);
    assert_eq!(row["match_any_rule"], true);
    assert_eq!(row["inverse"], false);
    let rs = rules(db, id).await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["reg_exp"], "hello");
    assert_eq!(rs[0]["filter_type"], 2);
    let acts = actions(db, id).await;
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["action_id"], 2);
}

/// `Pref_Filters::add` — a score action's `+N` param is stored as the integer.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_add_filter_score_action(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let req = request(&[
        ("title", json!("Score")),
        (
            "action",
            rule_param(&[json!({"action_id": 6, "action_param": "+5"})]),
        ),
    ]);
    ttrss::filters::add_filter(&state, &sess, &req)
        .await
        .expect("add");
    let id: i32 = sqlx::query_scalar("SELECT id FROM ttrss_filters2 WHERE title = 'Score'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let acts = actions(db, id).await;
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["action_id"], 6);
    assert_eq!(acts[0]["action_param"], "5");
}

/// `Pref_Filters::add` — a tag action's param is normalized (lowercased,
/// de-duplicated, sorted).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_add_filter_tag_action(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let req = request(&[
        ("title", json!("Tags")),
        (
            "action",
            rule_param(&[json!({"action_id": 4, "action_param": "World, Hello, world"})]),
        ),
    ]);
    ttrss::filters::add_filter(&state, &sess, &req)
        .await
        .expect("add");
    let id: i32 = sqlx::query_scalar("SELECT id FROM ttrss_filters2 WHERE title = 'Tags'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let acts = actions(db, id).await;
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["action_id"], 4);
    // normalize_categories: lowercase, dedupe, sort -> "hello, world".
    assert_eq!(acts[0]["action_param"], "hello, world");
}

// ---------------------------------------------------------------------------
// `clone`
// ---------------------------------------------------------------------------

/// `Pref_Filters::clone` — clones a filter with a `Clone of <title>` name,
/// copies its rules/actions, and disables it.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_clone_filters(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let src = insert_filter(db, "Source").await;
    sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, 'rule1', 1)").bind(src).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, 2, '')").bind(src).execute(db.pool()).await.unwrap();

    ttrss::filters::clone_filters(&state, &sess, &request(&[("ids", json!(src.to_string()))]))
        .await
        .expect("clone");

    assert_eq!(filter_count(db).await, 2);
    let clone_id: i32 =
        sqlx::query_scalar("SELECT id FROM ttrss_filters2 WHERE title = 'Clone of Source'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_ne!(clone_id, src);
    let row = filter_row(db, clone_id).await.unwrap();
    assert_eq!(row["enabled"], false);
    assert_eq!(rules(db, clone_id).await.len(), 1);
    assert_eq!(rules(db, clone_id).await[0]["reg_exp"], "rule1");
    assert_eq!(actions(db, clone_id).await.len(), 1);
    assert_eq!(actions(db, clone_id).await[0]["action_id"], 2);
    // The source is unchanged.
    assert_eq!(rules(db, src).await.len(), 1);

    // A custom new_filter_title is used when exactly one filter is cloned.
    let src2 = insert_filter(db, "Source2").await;
    ttrss::filters::clone_filters(
        &state,
        &sess,
        &request(&[
            ("ids", json!(src2.to_string())),
            ("new_filter_title", json!(" My Clone ")),
        ]),
    )
    .await
    .expect("clone2");
    let titled: i32 = sqlx::query_scalar("SELECT id FROM ttrss_filters2 WHERE title = 'My Clone'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_ne!(titled, src2);
}

// ---------------------------------------------------------------------------
// `join`
// ---------------------------------------------------------------------------

/// `Pref_Filters::join` — merges the selected filters into the first (base),
/// moves their rules/actions, sets match_any_rule, and de-dupes.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
))]
async fn test_join_filters(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    let base = insert_filter(db, "Base").await;
    let other = insert_filter(db, "Other").await;
    // Both filters share one identical rule + one identical action -> dupes.
    for fid in [base, other] {
        sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, 'same', 1)").bind(fid).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, 2, '')").bind(fid).execute(db.pool()).await.unwrap();
    }
    // Base has an extra (unique) rule.
    sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, 'unique', 1)").bind(base).execute(db.pool()).await.unwrap();

    ttrss::filters::join_filters(
        &state,
        &sess,
        &request(&[("ids", json!(format!("{base},{other}")))]),
    )
    .await
    .expect("join");

    // The base survives with match_any_rule set; the other is deleted.
    let row = filter_row(db, base).await.expect("base");
    assert_eq!(row["match_any_rule"], true);
    assert!(filter_row(db, other).await.is_none());
    // Rules merged + deduped: "same" (once) + "unique".
    let rs = rules(db, base).await;
    assert_eq!(rs.len(), 2);
    // Actions merged + deduped: the single shared action (once).
    assert_eq!(actions(db, base).await.len(), 1);
}
