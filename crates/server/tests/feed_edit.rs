//! DB tests for the read-side `Pref_Feeds::editfeed` form-data handler (M4:
//! feed CRUD).
//!
//! Rust-only: the frozen PHP suite has no `editfeed` form-data tests, so there
//! is no 1:1 parity mapping — these exercise the ported handler + its helpers
//! (`decrypt_feed_pass`, `get_icon`, `select_feeds_cats`, `get_ts_languages`) on
//! the versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4
//! (categories 1-2), owner 1 (access_level 10).

mod common;

use kuchikiki::parse_html;
use kuchikiki::traits::TendrilSink;
use axum::http::StatusCode;
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
        forbid_password_changes: false,
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
    // The full row is returned: all 39 columns + icon = 40 keys.
    let n_keys = feed.as_object().unwrap().len();
    assert_eq!(n_keys, 40, "feed should have all 39 columns + icon, got {n_keys}");
    // Spot-check a few internal columns the form does not display.
    assert_eq!(feed["owner_uid"], 1);
    assert_eq!(feed["update_method"], 0);
    assert!(feed["last_updated"].is_null(), "last_updated should be null: {feed}");

    // cats
    assert_eq!(result["cats"]["enabled"], true);
    let select = result["cats"]["select"].as_str().unwrap();
    // The <select> tag matches the PHP's multi-line format exactly.
    assert!(
        select.starts_with(
            "<select name=\"cat_id\"\n                        id=\"\"\n                        default=\"1\"\n                        dojoType=\"fox.form.Select\" >"
        ),
        "exact <select> tag: {select}"
    );
    assert!(select.contains("value='1'"), "Technology option missing: {select}");
    assert!(select.contains("Technology"));
    assert!(
        select.contains("Science &amp; Nature"),
        "& must be HTML-escaped: {select}"
    );
    assert!(select.contains("value=\"0\""), "Uncategorized option missing: {select}");
    assert!(select.contains("Uncategorized"));
    // The disabled separator is 15 U+2015 bars (matching the PHP).
    assert!(
        select.contains(&format!("<option disabled=\"1\">{}</option>", "\u{2015}".repeat(15))),
        "separator: {select}"
    );
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
    // "Technology" (top) comes before its child "\u{2003}Sub-Tech" (indented by an em space).
    let tech_pos = select.find("Technology").expect("Technology");
    let sub_pos = select.find("\u{2003}Sub-Tech").expect("indented Sub-Tech: {select}");
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

// --- editfeeds (batch-edit dialog) structural tests -----------------------
//
// The browser's HTML parser normalizes whitespace (the content is fed to a
// `dijit.Dialog` via `innerHTML`), so these tests parse the rendered HTML and
// assert the element/attribute structure and visible text rather than a
// byte-exact string (see AGENTS.md "Frontend HTML forms").

/// Parse an HTML fragment into a `kuchikiki` document root.
fn parse_doc(html: &str) -> kuchikiki::NodeRef {
    parse_html().one(html)
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeeds_overview(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html =
        ttrss_server::feeds::editfeeds(&state, &sess, &request(&[("ids", json!("1,2"))])).await;
    let doc = parse_doc(&html);

    // The three hidden fields (the form's identity).
    for (name, value) in [("ids", "1,2"), ("op", "Pref_Feeds"), ("method", "batchEditSave")] {
        let els = doc
            .select(&format!("input[name={name}]"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(els.len(), 1, "exactly one hidden field {name}");
        assert_eq!(
            els[0].attributes.borrow().get("value"),
            Some(value),
            "value of {name}"
        );
    }

    // The tab container and the three tabs in order.
    assert_eq!(
        doc.select("div[dojoType='dijit.layout.TabContainer']")
            .unwrap()
            .count(),
        1,
        "one tab container"
    );
    let tabs: Vec<String> = doc
        .select("div[dojoType='dijit.layout.ContentPane']")
        .unwrap()
        .map(|t| t.attributes.borrow().get("title").unwrap_or("").to_string())
        .collect();
    assert_eq!(
        tabs,
        vec![
            "General".to_string(),
            "Authentication".to_string(),
            "Options".to_string(),
        ]
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeeds_general_tab(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html =
        ttrss_server::feeds::editfeeds(&state, &sess, &request(&[("ids", json!("1,2"))])).await;
    let doc = parse_doc(&html);

    // The General tab's four selects, each paired with a "Check to enable
    // field" toggle.
    for name in ["cat_id", "feed_language", "update_interval", "purge_interval"] {
        let sel = doc
            .select(&format!("select[name={name}]"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(sel.len(), 1, "exactly one select {name}");

        let toggle = doc
            .select(&format!("input[data-control-for={name}]"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(toggle.len(), 1, "exactly one toggle for {name}");
        let attrs = toggle[0].attributes.borrow();
        assert_eq!(attrs.get("onchange"), Some("App.dialogOf(this).toggleField(this)"));
        assert_eq!(attrs.get("title"), Some("Check to enable field"));
    }

    // The update-interval select carries the default-annotation on key 0.
    let update_options: Vec<String> = doc
        .select("select[name=update_interval] option")
        .unwrap()
        .map(|o| {
            let v = o.attributes.borrow().get("value").unwrap_or("").to_string();
            format!("{v}={}", o.text_contents())
        })
        .collect();
    assert!(
        update_options.contains(&"0=Default interval (30 minutes)".to_string()),
        "{update_options:?}"
    );
    assert!(
        update_options.contains(&"-1=Disable updates".to_string()),
        "{update_options:?}"
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeeds_auth_tab(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html =
        ttrss_server::feeds::editfeeds(&state, &sess, &request(&[("ids", json!("1,2"))])).await;
    let doc = parse_doc(&html);

    // The Authentication tab's login + password inputs (disabled, autocomplete).
    let login = doc.select("input[name=auth_login]").unwrap().collect::<Vec<_>>();
    assert_eq!(login.len(), 1);
    let la = login[0].attributes.borrow();
    assert_eq!(la.get("disabled"), Some("1"));
    assert_eq!(la.get("autocomplete"), Some("new-password"));
    let pass = doc.select("input[name=auth_pass]").unwrap().collect::<Vec<_>>();
    assert_eq!(pass.len(), 1);
    let pa = pass[0].attributes.borrow();
    assert_eq!(pa.get("type"), Some("password"));
    assert_eq!(pa.get("disabled"), Some("1"));
    assert_eq!(pa.get("autocomplete"), Some("new-password"));

    // Each is paired with a toggle.
    for name in ["auth_login", "auth_pass"] {
        let toggle = doc
            .select(&format!("input[data-control-for={name}]"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(toggle.len(), 1, "exactly one toggle for {name}");
    }
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeeds_options_tab(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html =
        ttrss_server::feeds::editfeeds(&state, &sess, &request(&[("ids", json!("1,2"))])).await;
    let doc = parse_doc(&html);

    // The five boolean options, each a disabled checkbox + caption + toggle.
    let expected = [
        ("include_in_digest", "Include in email digest"),
        ("always_display_enclosures", "Always display image attachments"),
        ("hide_images", "Do not embed media"),
        ("cache_images", "Cache media"),
        ("mark_unread_on_update", "Mark updated articles as unread"),
    ];
    for (name, caption) in expected {
        let cb = doc.select(&format!("input[name={name}]")).unwrap().collect::<Vec<_>>();
        assert_eq!(cb.len(), 1, "exactly one checkbox {name}");
        assert_eq!(cb[0].attributes.borrow().get("disabled"), Some("1"));
        assert!(html.contains(caption), "caption {caption} present");
        let toggle = doc
            .select(&format!("input[data-control-for={name}]"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(toggle.len(), 1, "exactly one toggle for {name}");
    }

    // The footer has the Save (submit) button.
    assert_eq!(
        doc.select("button[type=submit]").unwrap().count(),
        1,
        "one submit button"
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_editfeeds_category_options(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let html =
        ttrss_server::feeds::editfeeds(&state, &sess, &request(&[("ids", json!("1,2"))])).await;
    let doc = parse_doc(&html);

    let options: Vec<(String, bool, bool)> = doc
        .select("select[name=cat_id] option")
        .unwrap()
        .map(|o| {
            let a = o.attributes.borrow();
            (
                o.text_contents(),
                a.get("disabled").is_some(),
                a.get("selected").is_some(),
            )
        })
        .collect();

    // The two fixture categories (top-level, no indent).
    let texts: Vec<&str> = options.iter().map(|(t, _, _)| t.as_str()).collect();
    assert!(texts.contains(&"Technology"), "{texts:?}");
    assert!(texts.contains(&"Science & Nature"), "{texts:?}");
    // The separator (15 U+2015) is a disabled option.
    let sep = "\u{2015}".repeat(15);
    assert!(
        options.iter().any(|(t, d, _)| t == &sep && *d),
        "separator disabled: {options:?}"
    );
    // "Uncategorized" is selected (default_id is None).
    assert!(
        options.iter().any(|(t, _, s)| t == "Uncategorized" && *s),
        "Uncategorized selected: {options:?}"
    );
}

/// `Feeds::subscribeToFeed` — the "Add a feed" dialog data: the category
/// `<select>` (named `cat`, the user's feed categories + "Uncategorized") and
/// the update-interval options (the default-annotated `$update_intervals`, a
/// JSON object). The frontend's `select_hash` re-sorts the interval keys, so
/// only the data (not the object key order) matters.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_subscribe_to_feed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    let result = ttrss_server::feeds::subscribe_to_feed(&state, &sess).await;

    // The category `<select>` — named "cat", listing the user's categories and
    // an "Uncategorized" option (selected, since default_id is None).
    let cat_select = result["cat_select"].as_str().expect("cat_select is a string");
    assert!(cat_select.contains("<select name=\"cat\""), "{cat_select}");
    assert!(cat_select.contains("value='1'>Technology</option>"), "{cat_select}");
    assert!(cat_select.contains("value='2'>Science &amp; Nature</option>"), "{cat_select}");
    assert!(cat_select.contains("value=\"0\">Uncategorized</option>"), "{cat_select}");

    // The update-interval options — a JSON object (key => label), with the
    // "Default interval" entry annotated with the DEFAULT_UPDATE_INTERVAL label
    // (no pref set in the fixtures → the default 30 → "30 minutes").
    let update = &result["intervals"]["update"];
    assert!(update.is_object(), "intervals.update is an object: {update}");
    assert_eq!(update["0"].as_str().unwrap(), "Default interval (30 minutes)");
    assert_eq!(update["-1"].as_str().unwrap(), "Disable updates");
    assert_eq!(update["15"].as_str().unwrap(), "15 minutes");
    assert_eq!(update["60"].as_str().unwrap(), "Hourly");
    assert_eq!(update["10080"].as_str().unwrap(), "Weekly");
}

/// `Feeds::opensite` — 302-redirect to a feed's site URL (validated), or 404 if
/// the feed is missing, owned by another user, or has an empty/invalid site
/// URL. Not a JSON method: it returns a raw status + optional `Location` + body.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_opensite(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // Extra feeds to exercise the failure paths:
    //   101 — owned by uid 2 (the session is uid 1) → not found
    //   102 — empty site_url → not found
    //   103 — invalid site_url → not found
    // The "other user" feed needs a second user to own it.
    sqlx::query("INSERT INTO ttrss_users (login, pwd_hash, access_level) VALUES ('other', 'x', 10)")
        .execute(db)
        .await
        .expect("insert second user");
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)\n\
         VALUES\n\
           (101, 2, 'Other user feed', 1, 'https://example.com/f101', 30, 0, 'https://example.com'),\n\
           (102, 1, 'Empty site', 1, 'https://example.com/f102', 30, 0, ''),\n\
           (103, 1, 'Bad site', 1, 'https://example.com/f103', 30, 0, 'not a url')",
    )
    .execute(db)
    .await
    .expect("insert test feeds");

    let extra = |feed_id: i32| {
        let mut m = Map::new();
        m.insert("feed_id".to_string(), json!(feed_id));
        m
    };

    // Valid site URL → 302 to the validated URL, no body.
    let (status, location, body) = ttrss_server::feeds::opensite(&state, &sess, &extra(1)).await;
    assert_eq!(status, StatusCode::FOUND, "valid feed redirects");
    assert_eq!(location.as_deref(), Some("https://news.ycombinator.com"));
    assert!(body.is_empty(), "redirect has no body");

    // Missing feed → 404 + message, no Location.
    let (status, location, body) = ttrss_server::feeds::opensite(&state, &sess, &extra(99)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(location, None);
    assert_eq!(body, "Feed not found or has an empty site URL.");

    // Feed owned by another user → 404 (the owner_uid filter excludes it).
    let (status, _, _) = ttrss_server::feeds::opensite(&state, &sess, &extra(101)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "other user's feed is not found");

    // Empty site URL → 404.
    let (status, _, _) = ttrss_server::feeds::opensite(&state, &sess, &extra(102)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "empty site URL is not found");

    // Invalid site URL → 404 (validation rejects it).
    let (status, _, _) = ttrss_server::feeds::opensite(&state, &sess, &extra(103)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "invalid site URL is not found");
}
