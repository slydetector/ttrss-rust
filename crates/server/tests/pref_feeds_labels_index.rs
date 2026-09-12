//! DB tests for the `Pref_Feeds::index` and `Pref_Labels::index` page
//! renderers (ported in `ef827a5` — the two missing prefs read-side tabs).
//! Rust-only: the frozen PHP suite has no index-page tests.
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets and the visible labels / menu items / control values must be
//! present; whitespace is not asserted.

mod common;

use serde_json::{Map, Value};
use ttrss_server::core::config::Config;
use ttrss_server::core::db::Db;
use ttrss_server::core::phpser::Value as PValue;
use ttrss_server::core::session::Session;
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
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        docroot: std::path::PathBuf::from("tt-rss"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (the fixture owner).
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

/// Strip all whitespace so structural comparisons are whitespace-agnostic.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

// ── Pref_Labels::index ───────────────────────────────────────────────────────

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
))]
async fn test_labels_index_structure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html = ttrss_server::labels::index(&state, &sess, &Map::new());
    let n = normalize(&html);

    // Layout: BorderContainer with a top toolbar pane + a center tree pane.
    assert!(n.contains("dijit.layout.BorderContainer"));
    assert!(n.contains("dijit.layout.ContentPane"));

    // Toolbar: Select drop-down (All/None), Create label, Remove, Clear colors.
    assert!(n.contains("fox.Toolbar"));
    assert!(n.contains("fox.form.DropDownButton"));
    assert!(n.contains("setAllChecked(true)"));
    assert!(n.contains("setAllChecked(false)"));
    assert!(n.contains("CommonDialogs.addLabel()"));
    assert!(n.contains("Createlabel"));
    assert!(n.contains("removeSelected()"));
    assert!(n.contains("Remove"));
    assert!(n.contains("resetColors()"));
    assert!(n.contains("Clearcolors"));

    // Tree: store fed by getlabeltree, the CheckBox model, and the tree itself.
    assert!(n.contains("jsId='labelStore'"));
    assert!(n.contains("backend.php?op=Pref_Labels&method=getlabeltree"));
    assert!(n.contains("jsId='labelModel'"));
    assert!(n.contains("lib.CheckBoxStoreModel"));
    assert!(n.contains("id='labelTree'"));
    assert!(n.contains("fox.PrefLabelTree"));
    // Clicking a label row opens the edit dialog.
    assert!(n.contains("editLabel(bare_id)"));
}

// ── Pref_Feeds::index ────────────────────────────────────────────────────────

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
))]
async fn test_feeds_index_structure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html = ttrss_server::feeds::index(&state, &sess, &Map::new()).await;
    let n = normalize(&html);

    // Layout: the left-positioned TabContainer with the three panes.
    assert!(n.contains("dijit.layout.TabContainer"));
    assert!(n.contains("title=\"<iclass='material-icons'>rss_feed</i>Myfeeds\""));
    assert!(n.contains("title=\"<iclass='material-icons'>import_export</i>OPML\""));
    assert!(n.contains("title=\"<iclass='material-icons'>share</i>Sharing\""));

    // "My feeds" toolbar: feed-tree search, Select drop-down, the Feeds menu
    // (ENABLE_FEED_CATS defaults to true → the Categories menu is present).
    assert!(n.contains("id=\"feed_search\""));
    assert!(n.contains("setAllChecked(true)"));
    assert!(n.contains("CommonDialogs.subscribeToFeed()"));
    assert!(n.contains("Subscribetofeed"));
    assert!(n.contains("editSelectedFeed()"));
    assert!(n.contains("Editselectedfeeds"));
    assert!(n.contains("batchSubscribe()"));
    assert!(n.contains("Batchsubscribe"));
    assert!(n.contains("removeSelectedFeeds()"));
    assert!(n.contains("Unsubscribe"));
    assert!(n.contains("createCategory()"));
    assert!(n.contains("Addcategory"));
    assert!(n.contains("pref_feeds_errors_btn"));
    assert!(n.contains("Feedswitherrors"));
    assert!(n.contains("pref_feeds_inactive_btn"));
    assert!(n.contains("Inactivefeeds"));

    // Feed tree: store fed by getfeedtree + the tree itself. No search term →
    // empty value, autoExpand false.
    assert!(n.contains("jsId=\"feedStore\""));
    assert!(n.contains("backend.php?op=Pref_Feeds&method=getfeedtree"));
    assert!(n.contains("id=\"feedTree\""));
    assert!(n.contains("fox.PrefFeedTree"));
    assert!(n.contains("value=\"\""));
    assert!(n.contains("autoExpand=\"false\""));

    // OPML pane: the import form (file input + hidden op/csrf/method + the
    // import button), the export form with the settings checkbox, and the
    // main-profile-only notice.
    assert!(n.contains("id='opml_import_form'"));
    assert!(n.contains("id='opml_file'"));
    assert!(n.contains("name='op'"));
    assert!(n.contains("name='csrf_token'"));
    assert!(n.contains("value='importOpml'"));
    assert!(n.contains("ImportOPML"));
    assert!(n.contains("id='opmlExportForm'"));
    assert!(n.contains("ExportOPML"));
    assert!(n.contains("include_settings"));
    assert!(n.contains("Includett-rsssettings"));
    assert!(n.contains("OnlymainsettingsprofilecanbemigratedusingOPML."));

    // Sharing pane: the generated-URL notice, the two buttons.
    assert!(n.contains("PublishedarticlescanbesubscribedbyanyonewhoknowsthefollowingURL:"));
    assert!(n.contains("generatedFeed(-2,false)"));
    assert!(n.contains("DisplayURL"));
    assert!(n.contains("clearFeedAccessKeys()"));
    assert!(n.contains("ClearallgeneratedURLs"));
}

// ── `search` term: request param + session cache ─────────────────────────────

// PHP `Pref_Feeds::index_feeds`: `$_REQUEST["search"]` when present is
// cleaned (strip_tags + trim), cached in the session as `prefs_feed_search`,
// and echoed into the search box (HTML-escaped); a later load without the
// param recalls the cached term. A non-empty term also sets autoExpand=true.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
))]
async fn test_feeds_index_search_term_and_cache(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    // 1. No search param → empty term, autoExpand false.
    let html = ttrss_server::feeds::index(&state, &sess, &Map::new()).await;
    let n = normalize(&html);
    assert!(n.contains("value=\"\""));
    assert!(n.contains("autoExpand=\"false\""));

    // 2. `search=a&b` → the term is cleaned (survives strip_tags), echoed
    //    HTML-escaped, and autoExpand flips to true.
    let mut extra = Map::new();
    extra.insert("search".into(), Value::String("a&b".into()));
    let html = ttrss_server::feeds::index(&state, &sess, &extra).await;
    let n = normalize(&html);
    assert!(
        n.contains("value=\"a&amp;b\""),
        "search term must be htmlspecialchars'd: {html}"
    );
    assert!(n.contains("autoExpand=\"true\""));

    // 3. No param again → the session-cached term is recalled.
    let html = ttrss_server::feeds::index(&state, &sess, &Map::new()).await;
    let n = normalize(&html);
    assert!(
        n.contains("value=\"a&amp;b\""),
        "cached search term must be recalled: {html}"
    );
    assert!(n.contains("autoExpand=\"true\""));

    // 4. An empty-string param clears the cached term.
    extra.insert("search".into(), Value::String("".into()));
    let html = ttrss_server::feeds::index(&state, &sess, &extra).await;
    let n = normalize(&html);
    assert!(n.contains("value=\"\""));
    assert!(n.contains("autoExpand=\"false\""));
}
