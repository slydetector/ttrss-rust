//! DB tests for the OPML **import** (M4: feed CRUD, OPML section).
//!
//! Rust-only: the frozen PHP suite has no OPML import tests, so these exercise
//! the ported `opml::import` directly on the versioned `schema` + `baseline` +
//! `seed` fixtures. The bar is the resulting DB state: categories created (and
//! deduped by title + parent), feeds inserted (deduped by `feed_url` + owner),
//! and the `tt-rss-prefs` / `tt-rss-labels` / `tt-rss-filters` settings sections
//! applied.

mod common;

use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

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

async fn make_state(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState {
        config: test_config(),
        db,
    }
}

/// A logged-in session for `uid` (mirrors the `opml_export.rs` setup).
fn sess_for(uid: i64) -> Sess {
    let mut data = ttrss_core::session::Session::new();
    data.set("uid", ttrss_core::phpser::Value::Int(uid));
    data.set("name", ttrss_core::phpser::Value::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess
}

async fn count(pool: &sqlx::PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .expect("count query")
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_creates_category_and_feed(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r#"<?xml version="1.0" encoding="utf-8"?>
<opml version="1.0">
  <head><title>test</title></head>
  <body>
    <outline text="New Cat">
      <outline type="rss" text="New Feed" xmlUrl="https://example.com/new" htmlUrl="https://example.com" ttrssUpdateInterval="45" ttrssSortOrder="3"/>
    </outline>
  </body>
</opml>"#;
    let result = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    assert!(result.get("rc").is_some(), "import ok: {result}");

    // The category is created.
    let cat: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = 'New Cat'")
            .fetch_optional(state.db.pool())
            .await
            .expect("cat query");
    let cat = cat.expect("New Cat created");

    // The feed is created, in the new category, with the imported settings.
    let row: Option<(Option<i32>, i32, i32)> = sqlx::query_as(
        "SELECT cat_id, order_id, update_interval FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/new'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("feed query");
    let (fcat, forder, fupdate) = row.expect("New Feed created");
    assert_eq!(fcat, Some(cat), "feed in New Cat");
    assert_eq!(forder, 3, "imported ttrssSortOrder");
    assert_eq!(fupdate, 45, "imported ttrssUpdateInterval");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_dedupes_feeds(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="Dup Cat">
        <outline type="rss" text="Dup Feed" xmlUrl="https://example.com/dup"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let first = count(state.db.pool(), "SELECT count(*) FROM ttrss_feeds WHERE feed_url = 'https://example.com/dup' AND owner_uid = 1").await;
    assert_eq!(first, 1, "first import creates one feed");

    // Import again: the feed (by url) and the category (by title) are both deduped.
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let second = count(state.db.pool(), "SELECT count(*) FROM ttrss_feeds WHERE feed_url = 'https://example.com/dup' AND owner_uid = 1").await;
    let cats = count(state.db.pool(), "SELECT count(*) FROM ttrss_feed_categories WHERE title = 'Dup Cat' AND owner_uid = 1").await;
    assert_eq!(second, 1, "no duplicate feed");
    assert_eq!(cats, 1, "no duplicate category");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_top_level_feed_uncategorized(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    // A top-level feed (not inside a category) goes to the "Imported feeds"
    // category if it exists, else NULL (uncategorized). No such category here.
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" text="Top Feed" xmlUrl="https://example.com/top"/>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let cat: Option<Option<i32>> = sqlx::query_scalar::<_, Option<i32>>("SELECT cat_id FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/top'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(cat, Some(None), "top-level feed is uncategorized when no 'Imported feeds' cat");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_top_level_feed_uses_imported_cat(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    // Pre-create the "Imported feeds" category: a top-level feed now lands in it.
    sqlx::query("INSERT INTO ttrss_feed_categories (owner_uid, title) VALUES (1, 'Imported feeds')")
        .execute(state.db.pool())
        .await
        .expect("insert Imported feeds");
    let imported_cat: i32 = sqlx::query_scalar("SELECT id FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = 'Imported feeds'")
        .fetch_one(state.db.pool())
        .await
        .expect("cat id");
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" text="Top Feed" xmlUrl="https://example.com/top"/>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let cat: Option<Option<i32>> = sqlx::query_scalar::<_, Option<i32>>("SELECT cat_id FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/top'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(cat, Some(Some(imported_cat)), "top-level feed lands in 'Imported feeds'");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_unknown_feed_title(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    // No text/title → the feed title defaults to "[Unknown]".
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" xmlUrl="https://example.com/untitled"/>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/untitled'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(title, Some("[Unknown]".to_string()), "untitled feed defaults to [Unknown]");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_parse_error_returns_error(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let result = ttrss_server::opml::import(&state, &sess, Some(b"this is not xml <<<")).await;
    assert!(result.get("error").is_some(), "parse error reported: {result}");
    assert!(result.get("error").is_some(), "parse error reported: {result}");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_labels(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r##"<opml version="1.0"><body>
      <outline text="tt-rss-labels">
        <outline label-name="My Label" label-fg-color="#ff0000" label-bg-color="#00ff00"/>
        <outline label-name="Second Label"/>
      </outline>
    </body></opml>"##;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;

    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT fg_color, bg_color FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'My Label'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("label query");
    let (fg, bg) = row.expect("My Label created");
    assert_eq!(fg, "#ff0000");
    assert_eq!(bg, "#00ff00");
    let second = count(state.db.pool(), "SELECT count(*) FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'Second Label'").await;
    assert_eq!(second, 1, "Second Label created (default colors)");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_labels_dedup(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-labels">
        <outline label-name="Dup Label"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let n = count(state.db.pool(), "SELECT count(*) FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'Dup Label'").await;
    assert_eq!(n, 1, "label deduped by caption");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_prefs(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-prefs">
        <outline pref-name="PURGE_OLD_DAYS" value="45"/>
        <outline pref-name="SHORT_DATE_FORMAT" value="Y-m-d"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;

    let purge: Option<String> = sqlx::query_scalar("SELECT value FROM ttrss_user_prefs2 WHERE owner_uid = 1 AND pref_name = 'PURGE_OLD_DAYS' AND profile IS NULL")
        .fetch_optional(state.db.pool())
        .await
        .expect("purge query");
    assert_eq!(purge, Some("45".to_string()), "PURGE_OLD_DAYS pref set");
    let fmt: Option<String> = sqlx::query_scalar("SELECT value FROM ttrss_user_prefs2 WHERE owner_uid = 1 AND pref_name = 'SHORT_DATE_FORMAT' AND profile IS NULL")
        .fetch_optional(state.db.pool())
        .await
        .expect("date format query");
    assert_eq!(fmt, Some("Y-m-d".to_string()), "SHORT_DATE_FORMAT pref set");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_import_filter(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let state = make_state(pool, options).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-filters">
        <outline filter-type="2"><![CDATA[{"match_any_rule":false,"enabled":true,"inverse":false,"title":"My Filter","rules":[{"reg_exp":"spam","filter_type":1,"cat_filter":false,"feed":"Hacker News","inverse":false}],"actions":[{"action_id":1,"action_param":""}]}]]></outline>
      </outline>
    </body></opml>"#;
    let _ = ttrss_server::opml::import(&state, &sess, Some(opml.as_bytes())).await;

    let filter_id: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_filters2 WHERE owner_uid = 1 AND title = 'My Filter' AND enabled = true AND match_any_rule = false",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("filter query");
    let filter_id = filter_id.expect("filter created");

    // The rule references the "Hacker News" feed (feed 1) by title.
    let rule: Option<(Option<i32>, i32)> = sqlx::query_as(
        "SELECT feed_id, filter_type FROM ttrss_filters2_rules WHERE filter_id = $1 AND reg_exp = 'spam'",
    )
    .bind(filter_id)
    .fetch_optional(state.db.pool())
    .await
    .expect("rule query");
    let (feed_id, ftype) = rule.expect("rule created");
    assert_eq!(feed_id, Some(1), "rule bound to Hacker News feed");
    assert_eq!(ftype, 1);

    let action: i64 = sqlx::query_scalar("SELECT count(*) FROM ttrss_filters2_actions WHERE filter_id = $1 AND action_id = 1")
        .bind(filter_id)
        .fetch_one(state.db.pool())
        .await
        .expect("action query");
    assert_eq!(action, 1, "action created");
}
