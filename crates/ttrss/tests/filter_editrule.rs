//! DB tests for the `Pref_Filters::editrule` feed multi-select and its
//! `_feed_multi_select` helper (M4). The expected HTML is byte-for-byte the
//! PHP oracle's output for the same nested dataset (see
//! `oracle/recorders/feed_multi_select.php`), covering the tree ordering, the
//! U+2003 em-space indentation, the `CAT:<id>` / feed-id / "All feeds" /
//! "Uncategorized" selection, and the `<select multiple=<TAB>rue"` opening quirk.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;

const EM: char = '\u{2003}';
const ATTRS: &str =
    "required=\"1\" style=\"width : 100%; height : 300px\" dojoType=\"fox.form.ValidationMultiSelect\"";

/// The fixed nested dataset (owner 1):
///   categories: 1 "Zeta" (top), 2 "Alpha" (top), 3 "Mid" (child of 2),
///               4 "Deep" (child of 3)
///   feeds:      10 "Feed A2" (cat 2), 11 "Feed A1" (cat 2), 12 "Feed M" (cat 3),
///               13 "Feed D" (cat 4), 14 "UnCat 1" (NULL), 15 "UnCat 2" (NULL),
///               16 "TopZ" (cat 1)
async fn insert_nested_dataset(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (id, owner_uid, title, parent_cat) VALUES \
         (1, 1, 'Zeta', NULL), (2, 1, 'Alpha', NULL), (3, 1, 'Mid', 2), (4, 1, 'Deep', 3)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url, cat_id) VALUES \
         (10, 1, 'Feed A2', 'u10', 2), (11, 1, 'Feed A1', 'u11', 2), \
         (12, 1, 'Feed M', 'u12', 3), (13, 1, 'Feed D', 'u13', 4), \
         (14, 1, 'UnCat 1', 'u14', NULL), (15, 1, 'UnCat 2', 'u15', NULL), \
         (16, 1, 'TopZ', 'u16', 1)",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The PHP oracle's full output for the nested dataset (no selection), with the
/// U+2003 em-space (`EM`) indentation and the tab in `<select multiple=<TAB>rue"`.
fn expected_nested(selected: &[&str]) -> String {
    // Returns `"<option  value=...>..."` (two spaces) when the value is not
    // selected, else `"<option selected=\"1\" value=...>..."`.
    let opt = |value: &str, title: &str, selected: bool| -> String {
        let sel = if selected { "selected=\"1\"" } else { "" };
        format!("<option {sel} value={value}>{title}</option>")
    };
    let in_ = |v: &str| selected.contains(&v);
    let body = [
        opt("\"0\"", "All feeds", in_("0")),
        opt("'CAT:2'", "Alpha", in_("CAT:2")),
        opt("'CAT:3'", &format!("{EM}Mid"), in_("CAT:3")),
        opt("'CAT:4'", &format!("{EM}{EM}Deep"), in_("CAT:4")),
        opt("'13'", &format!("{EM}{EM}{EM}Feed D"), in_("13")),
        opt("'12'", &format!("{EM}{EM}Feed M"), in_("12")),
        opt("'11'", &format!("{EM}Feed A1"), in_("11")),
        opt("'10'", &format!("{EM}Feed A2"), in_("10")),
        opt("'CAT:1'", "Zeta", in_("CAT:1")),
        opt("'16'", &format!("{EM}TopZ"), in_("16")),
        opt("'CAT:0'", "Uncategorized", in_("CAT:0")),
        opt("'14'", &format!("{EM}UnCat 1"), in_("14")),
        opt("'15'", &format!("{EM}UnCat 2"), in_("15")),
    ]
    .concat();
    format!(
        "<select multiple={tab}rue\" id=\"feed_id\" name=\"feed_id\" {attrs}>{body}</select>",
        tab = '\t',
        attrs = ATTRS
    )
}

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

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// `editrule` with no selection: the full category tree (title order, each
/// category then its whole subtree then its feeds), the "Uncategorized"
/// section, and the `<select multiple=<TAB>rue"` opening — byte-for-byte the PHP.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_editrule_nested(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    insert_nested_dataset(state.db.pool()).await;

    let result = ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!(""))])).await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&[]);
    assert_eq!(got, &want, "nested multi-select mismatch");
}

/// `editrule` with a selection: `0` (All feeds), feed `11`, and `CAT:3` get
/// `selected="1"`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_editrule_selected(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    insert_nested_dataset(state.db.pool()).await;

    let result =
        ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("0,11,CAT:3"))])).await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&["0", "11", "CAT:3"]);
    assert_eq!(got, &want, "selected multi-select mismatch");
}

/// `editrule` with category + feed selection: `CAT:2`, `CAT:4`, and feed `10`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_editrule_cat_selected(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    insert_nested_dataset(state.db.pool()).await;

    let result =
        ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("CAT:2,CAT:4,10"))]))
            .await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&["CAT:2", "CAT:4", "10"]);
    assert_eq!(got, &want, "cat-selected multi-select mismatch");
}

/// `editrule` with `ENABLE_FEED_CATS` disabled: a flat, title-ordered feed list
/// (no "All feeds", no "Uncategorized", no indentation) inside the same `<select>`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_editrule_flat(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();
    insert_nested_dataset(db).await;
    // Disable feed categories (a user pref override for the default profile).
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, 'ENABLE_FEED_CATS', NULL, 'false')",
    )
    .execute(db)
    .await
    .unwrap();

    let result = ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("11"))])).await;
    let got = result["multiselect"].as_str().unwrap();

    // Flat list, title order: All feeds (include_all_feeds) then Feed A1, Feed A2,
    // Feed D, Feed M, TopZ, UnCat 1, UnCat 2 — no "Uncategorized", no em-space
    // indentation, no categories.
    let want = [
        format!(
            "<select multiple={tab}rue\" id=\"feed_id\" name=\"feed_id\" {attrs}>",
            tab = '\t',
            attrs = ATTRS
        ),
        "<option  value=\"0\">All feeds</option>".to_string(),
        "<option  value='11'>Feed A1</option>".to_string(),
        "<option  value='10'>Feed A2</option>".to_string(),
        "<option  value='13'>Feed D</option>".to_string(),
        "<option  value='12'>Feed M</option>".to_string(),
        "<option  value='16'>TopZ</option>".to_string(),
        "<option  value='14'>UnCat 1</option>".to_string(),
        "<option  value='15'>UnCat 2</option>".to_string(),
        "</select>".to_string(),
    ]
    .join("");
    // The selected feed (11) gets selected="1".
    let want = want.replace("<option  value='11'>", "<option selected=\"1\" value='11'>");
    assert_eq!(got, &want, "flat multi-select mismatch");
}
