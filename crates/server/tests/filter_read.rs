//! DB tests for the `Pref_Filters` read side (M4): `edit` (the edit-dialog
//! prefill), `printRuleName`, and `printActionName`.
//!
//! Rust-only: the frozen PHP suite has no `Pref_Filters` configuration tests,
//! so there is no 1:1 parity mapping — these exercise the ported handlers on
//! the versioned `schema` + `baseline` + `seed` fixtures (feeds 1-4,
//! categories 1-2, owner 1). Owner 1 is the test session's uid.

mod common;

use serde_json::{json, Map, Value};
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

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
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
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

/// Insert a bare filter row (owner 1) and return its id.
async fn insert_filter(db: &Db, title: &str) -> i32 {
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, title, enabled, match_any_rule, inverse) \
         VALUES (1, $1, true, false, false) RETURNING id",
    )
    .bind(title)
    .fetch_one(db.pool())
    .await
    .expect("insert filter");
    id
}

/// `edit` with `id = 0` returns the empty-filter defaults: enabled, no rules /
/// actions, and the `filter_types` / `action_types` maps (type 5 unsupported).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql"
))]
async fn test_edit_new_filter(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let Some(json) = ttrss_server::filters::edit(&state, &sess, &request(&[("id", json!(0))])).await
    else {
        panic!("expected Some for a new filter");
    };
    assert_eq!(json["id"], 0);
    assert_eq!(json["enabled"], true);
    assert_eq!(json["match_any_rule"], false);
    assert_eq!(json["inverse"], false);
    assert_eq!(json["title"], "");
    assert_eq!(json["rules"].as_array().unwrap().len(), 0);
    assert_eq!(json["actions"].as_array().unwrap().len(), 0);
    assert_eq!(json["filter_types"]["1"], "Title");
    assert_eq!(json["filter_types"]["2"], "Content");
    assert_eq!(json["filter_types"]["3"], "Title or Content");
    assert_eq!(json["filter_types"]["4"], "Link");
    assert_eq!(json["filter_types"]["6"], "Author");
    assert_eq!(json["filter_types"]["7"], "Article Tags");
    assert!(json["filter_types"].get("5").is_none(), "type 5 is unsupported");
    assert_eq!(json["action_types"]["1"], "Delete article");
    assert_eq!(json["action_types"]["4"], "Assign tags");
    assert_eq!(json["action_types"]["7"], "Assign label");
    assert_eq!(json["action_types"]["10"], "Ignore tags");
    assert!(json["plugin_actions"].is_object());
    assert!(json["labels"].is_array());
}

/// `edit` on a missing filter (nonzero id, not owned) returns `None`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql"
))]
async fn test_edit_not_found(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let json = ttrss_server::filters::edit(&state, &sess, &request(&[("id", json!(999999))])).await;
    assert!(json.is_none(), "missing filter should print nothing");
}

/// `edit` on a filter with rules + actions returns the full prefill: the rule
/// list (with computed names, feed ids, and `inverse` omitted when false), the
/// action list (with `action_param_label` and names), and the type maps.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql"
))]
async fn test_edit_existing(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();
    let fid = insert_filter(&state.db, "My filter").await;

    // A title rule on feed 1 (Hacker News) and a tag rule on all feeds.
    sqlx::query(
        "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type, feed_id, cat_id, \
         match_on, cat_filter, inverse) VALUES \
         ($1, '(?i)rust', 1, 1, NULL, NULL, false, false), \
         ($1, '(?i)tag', 7, NULL, NULL, NULL, false, false)",
    )
    .bind(fid)
    .execute(db)
    .await
    .unwrap();
    // A tag action and a score action.
    sqlx::query(
        "INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES \
         ($1, 4, 'hello, world'), ($1, 6, '5')",
    )
    .bind(fid)
    .execute(db)
    .await
    .unwrap();

    let Some(json) =
        ttrss_server::filters::edit(&state, &sess, &request(&[("id", json!(fid))])).await
    else {
        panic!("expected Some");
    };
    assert_eq!(json["id"], fid);
    assert_eq!(json["title"], "My filter");
    assert_eq!(json["enabled"], true);

    let rules = json["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2, "two rules: {json}");
    // Ordered by reg_exp: "(?i)rust" < "(?i)tag".
    assert_eq!(rules[0]["reg_exp"], "(?i)rust");
    assert_eq!(rules[0]["filter_type"], 1);
    assert_eq!(rules[0]["feed_id"], json!(["1"]));
    assert!(rules[0].get("inverse").is_none(), "inverse false is omitted");
    assert!(rules[0]["name"].as_str().unwrap().contains("Hacker News"));
    assert!(rules[0]["name"].as_str().unwrap().contains("Title"));

    assert_eq!(rules[1]["reg_exp"], "(?i)tag");
    assert_eq!(rules[1]["filter_type"], 7);
    assert_eq!(rules[1]["feed_id"], json!(["0"]), "no feed => \"All feeds\" (0)");
    assert!(rules[1]["name"].as_str().unwrap().contains("All feeds"));

    let actions = json["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2, "two actions: {json}");
    assert_eq!(actions[0]["action_id"], 4);
    assert_eq!(actions[0]["action_param"], "hello, world");
    assert_eq!(actions[0]["action_param_label"], "hello, world");
    assert_eq!(actions[0]["name"], "Assign tags: hello, world");
    assert_eq!(actions[1]["action_id"], 6);
    assert_eq!(actions[1]["name"], "Modify score: 5");
}

/// `printRuleName` renders the HTML rule name; a non-object / invalid JSON
/// yields the PHP's literal error strings.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql"
))]
async fn test_print_rule_name(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let rule = json!({ "reg_exp": "(?i)rust", "filter_type": 1, "feed_id": ["1"] });
    let name = ttrss_server::filters::print_rule_name(
        &state,
        &sess,
        &request(&[("rule", json!(rule.to_string()))]),
    )
    .await;
    assert!(
        name.contains("(?i)rust on <span class='field'>Title</span> in <span class='feed'>Hacker News</span>"),
        "got {name}"
    );
    assert!(name.starts_with("<span class='filterRule '"), "got {name}");

    // A non-object JSON decodes to "invalid rule JSON".
    let name = ttrss_server::filters::print_rule_name(
        &state,
        &sess,
        &request(&[("rule", json!("123"))]),
    )
    .await;
    assert_eq!(name, "invalid rule JSON");

    // An unparseable string is "malformed rule JSON".
    let name = ttrss_server::filters::print_rule_name(
        &state,
        &sess,
        &request(&[("rule", json!("{not json"))]),
    )
    .await;
    assert_eq!(name, "malformed rule JSON");
}

/// `printActionName` renders the action name; a missing / non-object param
/// yields the PHP's literal error strings.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql"
))]
async fn test_print_action_name(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let action = json!({ "action_id": 4, "action_param": "hello, world" });
    let name = ttrss_server::filters::print_action_name(
        &state,
        &sess,
        &request(&[("action", json!(action.to_string()))]),
    )
    .await;
    assert_eq!(name, "Assign tags: hello, world");

    // A label action appends the label name.
    let action = json!({ "action_id": 7, "action_param": "Work" });
    let name = ttrss_server::filters::print_action_name(
        &state,
        &sess,
        &request(&[("action", json!(action.to_string()))]),
    )
    .await;
    assert_eq!(name, "Assign label: Work");

    // A non-object JSON decodes to "invalid action JSON".
    let name = ttrss_server::filters::print_action_name(
        &state,
        &sess,
        &request(&[("action", json!("42"))]),
    )
    .await;
    assert_eq!(name, "invalid action JSON");
}
