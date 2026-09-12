//! DB tests for the `Pref_Filters::index` handler — the filters tab page (the
//! Dojo UI the preferences tab injects). Rust-only: the frozen PHP suite has no
//! `Pref_Filters` configuration tests, so these exercise the ported handler on
//! the versioned `schema` + `baseline` fixtures (owner 1 = the session uid).
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets (`id` / `jsId` / `dojoType`) and the visible button labels must be
//! present; whitespace is not asserted.

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

/// `index` renders the filters tab page: the Dojo tree / store widgets and the
/// toolbar button labels (`__()` is the identity) are present, and the search
/// box is empty when there is no search.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_index_structure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html = ttrss::filters::index(&state, &sess, &request(&[])).await;

    // The Dojo widgets the front-end relies on (structure, not whitespace).
    assert!(html.contains("<div dojoType='dijit.layout.BorderContainer' gutters='false'>"));
    assert!(html.contains("id=\"filter_search\""));
    assert!(html.contains("jsId=\"filterStore\""));
    assert!(html.contains("jsId=\"filterModel\""));
    assert!(html.contains("id=\"filterTree\""));
    assert!(html.contains("url=\"backend.php?op=Pref_Filters&method=getfiltertree\""));
    assert!(html.contains("rootLabel=\"Filters\""));

    // The toolbar button / menu labels (`__()` identity).
    for label in [
        "Search",
        "Select",
        "All",
        "None",
        "Create filter",
        "Clone",
        "Combine",
        "Remove",
        "Reset sort order",
        "Toggle rule display",
    ] {
        assert!(html.contains(label), "missing label {label:?}");
    }

    // No search → empty search box.
    assert!(html.contains("value=\"\""));
}

/// `index` with a `search` param: the value is `clean()`ed, html-escaped in the
/// output, and stored in the session (`prefs_filter_search`) as the clean
/// (unescaped) value — matching PHP.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_index_search_param(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html = ttrss::filters::index(
        &state,
        &sess,
        &request(&[("search", json!("rust & \"dev\""))]),
    )
    .await;

    // html-escaped in the output.
    assert!(html.contains("value=\"rust &amp; &quot;dev&quot;\""));
    // stored in the session unescaped (the clean value).
    assert_eq!(
        sess.data().get_str("prefs_filter_search"),
        Some("rust & \"dev\"")
    );
}

/// `index` without a `search` param reads `prefs_filter_search` from the session.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_index_search_from_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    sess.with_mut(|s| {
        s.set(
            "prefs_filter_search",
            PValue::Bytes(b"prior search".to_vec()),
        )
    });
    let html = ttrss::filters::index(&state, &sess, &request(&[])).await;
    assert!(html.contains("value=\"prior search\""));
}
