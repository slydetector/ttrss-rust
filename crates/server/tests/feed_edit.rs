//! DB tests for the read-side `Pref_Feeds::editfeed` form-data handler (M4:
//! feed CRUD).
//!
//! Rust-only: the frozen PHP suite has no `editfeed` form-data tests, so there
//! is no 1:1 parity mapping — these exercise the ported handler + its helpers
//! (`decrypt_feed_pass`, `get_icon`, `select_feeds_cats`, `get_ts_languages`) on
//! the versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4
//! (categories 1-2), owner 1 (access_level 10).

mod common;

use base64::Engine as _;
use serde_json::{json, Map, Value};
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::{self, Session};
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
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
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

/// `editfeed` returns the feed row (with a computed `icon` + decrypted
/// `auth_pass`), the category `<select>`, the update/purge intervals, the user
/// access level, and the text-search language list.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeed_basic(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let result =
        ttrss_server::feeds::editfeed(&state, &sess, &request(&[("id", json!(1))])).await;
    assert!(result.is_object(), "expected object, got {result}");

    let feed = &result["feed"];
    assert_eq!(feed["id"], 1);
    assert_eq!(feed["title"], "Hacker News");
    assert_eq!(feed["feed_url"], "https://hnrss.org/frontpage");
    assert_eq!(feed["site_url"], "https://news.ycombinator.com");
    assert_eq!(feed["cat_id"], 1);
    assert_eq!(feed["update_interval"], 30);
    assert_eq!(feed["purge_interval"], 0);
    assert!(feed["icon"].is_string(), "icon should be a string");
    assert_eq!(feed["auth_pass"], "", "empty auth_pass stays empty");

    // cats
    assert_eq!(result["cats"]["enabled"], true);
    let select = result["cats"]["select"].as_str().unwrap();
    assert!(select.contains("<select name=\"cat_id\""), "{select}");
    assert!(select.contains("value='1'"), "Technology option missing: {select}");
    assert!(select.contains("Technology"));
    assert!(
        select.contains("Science &amp; Nature"),
        "& must be HTML-escaped: {select}"
    );
    assert!(select.contains("value=\"0\""), "Uncategorized option missing: {select}");
    assert!(select.contains("Uncategorized"));
    // feed 1's cat is 1, so Technology is pre-selected.
    assert!(
        select.contains("selected=\"1\" value='1'"),
        "cat 1 should be selected: {select}"
    );

    // intervals
    assert!(result["intervals"]["update"].is_object());
    assert!(result["intervals"]["update"]["30"]
        .as_str()
        .unwrap()
        .contains("30 minutes"));
    let update0 = result["intervals"]["update"]["0"].as_str().unwrap();
    assert!(
        update0.starts_with("Default interval"),
        "default interval annotation: {update0}"
    );
    assert!(update0.contains("30 minutes"), "{update0}");
    assert!(result["intervals"]["purge"].is_object());
    let purge0 = result["intervals"]["purge"]["0"].as_str().unwrap();
    assert!(purge0.starts_with("Use default"), "{purge0}");
    assert!(purge0.contains("(60 days)"), "default purge suffix: {purge0}");
    assert_eq!(result["force_purge"], 0);

    // user
    assert_eq!(result["user"]["access_level"], 10);

    // lang
    assert_eq!(result["lang"]["enabled"], true);
    let all = result["lang"]["all"].as_array().unwrap();
    assert!(!all.is_empty(), "expected at least one ts language");
    for l in all {
        let s = l.as_str().unwrap();
        assert!(!s.is_empty(), "language entry should be non-empty");
        // `ucfirst` uppercases the first char; verify the first char is upper.
        let first = s.chars().next().unwrap();
        assert_eq!(
            first,
            first.to_uppercase().collect::<String>().chars().next().unwrap(),
            "language should be ucfirst: {s}"
        );
    }

    // plugin_data
    assert_eq!(result["plugin_data"], "");
}

/// `editfeed` returns `null` for an unknown / non-owned feed.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeed_unknown_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let result =
        ttrss_server::feeds::editfeed(&state, &sess, &request(&[("id", json!(9999))])).await;
    assert!(result.is_null(), "expected null, got {result}");
}

/// Nested categories are indented by level in the category `<select>`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeed_cats_nested(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;
    // Add a child category under "Technology" (id 1).
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (owner_uid, title, parent_cat) VALUES (1, 'Sub-Tech', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let result =
        ttrss_server::feeds::editfeed(&state, &sess, &request(&[("id", json!(1))])).await;
    let select = result["cats"]["select"].as_str().unwrap();
    // "Technology" (top) comes before its child " Sub-Tech" (indented by one space).
    let tech_pos = select.find("Technology").expect("Technology");
    let sub_pos = select.find(" Sub-Tech").expect("indented Sub-Tech: {select}");
    assert!(sub_pos > tech_pos, "child should follow parent: {select}");
    assert!(select.contains("Sub-Tech"));
}

/// `decrypt_feed_pass` reverses the `editSave` auth_pass envelope.
#[test]
fn test_decrypt_feed_pass_roundtrip() {
    let key = [0x42u8; 32];
    let env = session::encrypt_envelope(b"super-secret".as_ref(), &key).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(env);
    let decrypted = ttrss_server::feeds::decrypt_feed_pass(&encoded, Some(&key));
    assert_eq!(decrypted, "super-secret");

    // A non-encoded value (e.g. a plain password) is returned unchanged.
    assert_eq!(
        ttrss_server::feeds::decrypt_feed_pass("plain-pass", Some(&key)),
        "plain-pass"
    );
    // No key → returned unchanged.
    assert_eq!(
        ttrss_server::feeds::decrypt_feed_pass(&encoded, None),
        encoded
    );
}

/// `get_icon` maps virtual feed ids to Material icon names.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_icon_virtual(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, _sess) = setup(pool, options).await;
    use ttrss_server::feeds::get_icon;
    assert_eq!(get_icon(&state, 0), "archive");
    assert_eq!(get_icon(&state, -1), "star");
    assert_eq!(get_icon(&state, -2), "rss_feed");
    assert_eq!(get_icon(&state, -3), "whatshot");
    assert_eq!(get_icon(&state, -4), "inbox");
    assert_eq!(get_icon(&state, -6), "restore");
    // Label ids (below LABEL_BASE_INDEX) map to the generic label icon.
    assert_eq!(get_icon(&state, ttrss_server::labels::LABEL_BASE_INDEX - 5), "label");
    // A real feed with no icon file maps to the empty string.
    assert_eq!(get_icon(&state, 1), "");
}
