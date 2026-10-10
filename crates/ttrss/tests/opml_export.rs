//! DB tests for the OPML **export** (M4: feed CRUD, OPML section).
//!
//! Rust-only: the frozen PHP suite has no `OPML` export tests, so these
//! exercise the ported `opml::build_opml` / `opml::export` directly on the
//! versioned `schema` + `baseline` + `seed` fixtures: feeds 1-4 in categories
//! 1-2, labels 1-2, for owner 1. The bar is *functionally equivalent* OPML
//! (same feeds/categories/settings), not a byte-for-byte DOMDocument match.

mod common;

use test_db::template::with_test_db;
use ttrss::session::Sess;

/// The feed-tree export (no settings): both categories, all four feeds, each
/// nested under its category, with `htmlUrl`, and *no* `ttrss*` attributes.
#[tokio::test]
async fn test_export_feed_tree() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let opml = ttrss::opml::build_opml(state.db.pool(), 1, false).await;

    // Envelope.
    assert!(
        opml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
        "xml decl: {opml}"
    );
    assert!(opml.contains("<opml version=\"1.0\">"));
    assert!(opml.contains("<title>Tiny Tiny RSS Feed Export</title>"));

    // Both categories, at depth 2 (4-space indent), `&` escaped.
    assert!(
        opml.contains("    <outline text=\"Technology\">"),
        "Technology cat: {opml}"
    );
    assert!(
        opml.contains("    <outline text=\"Science &amp; Nature\">"),
        "Science cat: {opml}"
    );

    // All four feeds, nested at depth 3 (6-space indent) with xmlUrl + htmlUrl.
    assert!(opml.contains("      <outline type=\"rss\" text=\"Hacker News\" xmlUrl=\"https://hnrss.org/frontpage\" htmlUrl=\"https://news.ycombinator.com\"/>"), "HN feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"The Register\" xmlUrl=\"https://www.theregister.com/headlines.atom\" htmlUrl=\"https://www.theregister.com\"/>"), "Register feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"Ars Technica\" xmlUrl=\"https://feeds.arstechnica.com/arstechnica/index\" htmlUrl=\"https://arstechnica.com\"/>"), "Ars feed: {opml}");
    assert!(opml.contains("      <outline type=\"rss\" text=\"MIT Tech Review\" xmlUrl=\"https://www.technologyreview.com/feed/\" htmlUrl=\"https://www.technologyreview.com\"/>"), "MIT feed: {opml}");

    // No settings → no ttrss* attributes.
    assert!(!opml.contains("ttrssSortOrder"), "no ttrss attrs: {opml}");
    assert!(!opml.contains("tt-rss-prefs"));
        Ok(())
    })).await.unwrap();
}

/// With `include_settings`, the feed outlines carry the `ttrss*` attributes and
/// the category carries `ttrssSortOrder`.
#[tokio::test]
async fn test_export_include_settings() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let opml = ttrss::opml::build_opml(state.db.pool(), 1, true).await;

    // Category sort order.
    assert!(
        opml.contains("    <outline text=\"Technology\" ttrssSortOrder=\"0\">"),
        "cat sort: {opml}"
    );

    // Feed ttrss* attrs (Hacker News has update_interval=30, order_id=0, purge=0).
    assert!(
        opml.contains("      <outline type=\"rss\" text=\"Hacker News\" xmlUrl=\"https://hnrss.org/frontpage\" ttrssSortOrder=\"0\" ttrssPurgeInterval=\"0\" ttrssUpdateInterval=\"30\" htmlUrl=\"https://news.ycombinator.com\"/>"),
        "HN ttrss: {opml}"
    );
    // MIT Tech Review has update_interval=120.
    assert!(
        opml.contains("ttrssUpdateInterval=\"120\""),
        "MIT ttrss: {opml}"
    );
        Ok(())
    })).await.unwrap();
}

/// The `export` handler returns the OPML as a download (content-type + filename).
#[tokio::test]
async fn test_export_handler() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let mut data = ttrss::core::session::Session::new();
            data.set("uid", ttrss::core::phpser::Value::Int(1));
            data.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
            let sess = Sess::new("test-sid".to_string());
            sess.replace("test-sid".to_string(), data);

            let extra = serde_json::Map::new();
            let (status, ct, disp, body) = ttrss::opml::export(&state, &sess, &extra).await;

            assert_eq!(status, axum::http::StatusCode::OK);
            assert_eq!(ct, "application/xml+opml");
            let disp = disp.expect("content-disposition");
            assert!(
                disp.starts_with("attachment; filename=tt-rss_admin_"),
                "disposition: {disp}"
            );
            assert!(disp.ends_with(".opml"), "disposition: {disp}");
            assert!(body.contains("<opml version=\"1.0\">"), "body: {body}");
            assert!(body.contains("text=\"Hacker News\""), "body: {body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Empty categories are RETAINED. PHP `opml_export_category` emits every
/// category that has a title (feeds or not); the "cleanup empty categories"
/// pass it runs afterwards is a no-op, because it queries `//outline[@title]`
/// while every exported outline uses `text` (not `title`). A functionally
/// equivalent export must reproduce this so an empty category round-trips on
/// import.
#[tokio::test]
async fn test_export_retains_empty_category() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            // Add an empty category (no feeds, no sub-cats).
            sqlx::query(
                "INSERT INTO ttrss_feed_categories (owner_uid, title) VALUES (1, 'Empty Cat')",
            )
            .execute(state.db.pool())
            .await
            .expect("insert empty cat");

            let opml = ttrss::opml::build_opml(state.db.pool(), 1, false).await;
            // The empty category is emitted as a self-closing outline (no children).
            assert!(
                opml.contains("    <outline text=\"Empty Cat\"/>"),
                "empty category retained: {opml}"
            );
            // The real categories remain.
            assert!(opml.contains("text=\"Technology\""));
            assert!(opml.contains("text=\"Science &amp; Nature\""));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Settings round trip: export owner 1's prefs/labels/filters with
/// `include_settings`, import the result back as owner 1, and assert the settings
/// survive. This is the interop bar the Rust export must meet — the export mirrors
/// the PHP export, so the same file is equally consumable by PHP. The filter rule's
/// `reg_exp` carries `&`/`<`/`>` on purpose: it proves the payload is a real CDATA
/// section (an entity-escaped payload would corrupt it, since the import's
/// `Event::Text` handler is not entity-decoded).
#[tokio::test]
async fn test_export_import_settings_roundtrip() {
    with_test_db(|url| Box::pin(async move {
      let state = common::make_app_state(&url).await;
      let pool = state.db.pool();

      // Seed known settings for owner 1 (unique names keep the assertions precise).
      sqlx::query(
          "INSERT INTO ttrss_labels2 (owner_uid, caption, fg_color, bg_color) VALUES (1, 'RT Label', '#111111', '#222222')",
      )
      .execute(pool)
      .await
      .expect("label");
      sqlx::query(
          "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) VALUES (1, 'RT_TEST_PREF', NULL, 'rt_value')",
      )
      .execute(pool)
      .await
      .expect("pref");
      let fid: i32 = sqlx::query_scalar(
          "INSERT INTO ttrss_filters2 (owner_uid, title, enabled, match_any_rule, inverse) VALUES (1, 'RT Filter', true, false, false) RETURNING id",
      )
      .fetch_one(pool)
      .await
      .expect("filter");
      sqlx::query(
          "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type, feed_id, cat_filter, inverse) VALUES ($1, 'rt<&regex>', 1, 1, false, false)",
      )
      .bind(fid)
      .execute(pool)
      .await
      .expect("rule");
      sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, 1, '')")
          .bind(fid)
          .execute(pool)
      .await
      .expect("action");

      // 1. Export owner 1 with settings.
      let opml = ttrss::opml::build_opml(pool, 1, true).await;

      // The three settings blocks, each carrying the schema version.
      let want = format!("text=\"tt-rss-prefs\" schema-version=\"{}\"", ttrss::core::config::SCHEMA_VERSION);
        assert!(opml.contains(&want), "prefs block: {opml}");
      let want = format!("text=\"tt-rss-labels\" schema-version=\"{}\"", ttrss::core::config::SCHEMA_VERSION);
        assert!(opml.contains(&want), "labels block: {opml}");
      let want = format!("text=\"tt-rss-filters\" schema-version=\"{}\"", ttrss::core::config::SCHEMA_VERSION);
        assert!(opml.contains(&want), "filters block: {opml}");
      assert!(opml.contains("pref-name=\"RT_TEST_PREF\" value=\"rt_value\""), "pref row: {opml}");
      assert!(opml.contains("label-name=\"RT Label\" label-fg-color=\"#111111\" label-bg-color=\"#222222\""), "label row: {opml}");
      // The filter is a CDATA section (not entity-escaped), with native JSON types
      // and the feed match rendered as its portable title.
      assert!(opml.contains("<![CDATA[{\""), "filter CDATA: {opml}");
      assert!(opml.contains("\"title\":\"RT Filter\""), "filter title: {opml}");
      assert!(opml.contains("\"match_any_rule\":false"), "native bool: {opml}");
      assert!(opml.contains("\"enabled\":true"), "native bool: {opml}");
      assert!(opml.contains("\"reg_exp\":\"rt<&regex>\""), "CDATA keeps &/< verbatim: {opml}");
      assert!(opml.contains("\"feed\":\"Hacker News\""), "feed match -> title: {opml}");
      assert!(opml.contains("\"action_id\":1"), "native int: {opml}");

      // 2. Import the export back as owner 1.
      let mut data = ttrss::core::session::Session::new();
      data.set("uid", ttrss::core::phpser::Value::Int(1));
      data.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
      let sess = Sess::new("test-sid".to_string());
      sess.replace("test-sid".to_string(), data);
      let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

      // 3. The settings came back.
      // Label (deduped by caption; the seeded row keeps its colors).
      let (fg, bg): (String, String) = sqlx::query_as(
          "SELECT fg_color, bg_color FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'RT Label'",
      )
      .fetch_one(pool)
      .await
      .expect("label round-tripped");
      assert_eq!(fg, "#111111");
      assert_eq!(bg, "#222222");
      // Pref (upserted by name).
      let pref: Option<String> = sqlx::query_scalar(
          "SELECT value FROM ttrss_user_prefs2 WHERE owner_uid = 1 AND pref_name = 'RT_TEST_PREF' AND profile IS NULL",
      )
      .fetch_optional(pool)
      .await
      .expect("pref query");
      assert_eq!(pref, Some("rt_value".to_string()));
      // Filter (not deduped: a new 'RT Filter' row, rule re-resolved, action added).
      let n_filters: i64 = sqlx::query_scalar(
          "SELECT count(*) FROM ttrss_filters2 WHERE owner_uid = 1 AND title = 'RT Filter'",
      )
      .fetch_one(pool)
      .await
      .expect("filter count");
      assert_eq!(n_filters, 2, "original + imported filter");
      let n_rules: i64 = sqlx::query_scalar(
          "SELECT count(*) FROM ttrss_filters2_rules r JOIN ttrss_filters2 f ON f.id = r.filter_id WHERE f.owner_uid = 1 AND f.title = 'RT Filter'",
      )
      .fetch_one(pool)
      .await
      .expect("rule count");
      assert_eq!(n_rules, 2, "original + imported rule");
      let (reg_exp, ftype, feed_id): (String, i32, Option<i32>) = sqlx::query_as(
          "SELECT r.reg_exp, r.filter_type, r.feed_id FROM ttrss_filters2_rules r JOIN ttrss_filters2 f ON f.id = r.filter_id WHERE f.owner_uid = 1 AND f.title = 'RT Filter' LIMIT 1",
      )
      .fetch_optional(pool)
      .await
      .expect("rule row")
      .expect("rule exists");
      assert_eq!(reg_exp, "rt<&regex>", "CDATA payload not corrupted");
      assert_eq!(ftype, 1);
      assert_eq!(feed_id, Some(1), "feed title re-resolved to Hacker News");
      let n_actions: i64 = sqlx::query_scalar(
          "SELECT count(*) FROM ttrss_filters2_actions a JOIN ttrss_filters2 f ON f.id = a.filter_id WHERE f.owner_uid = 1 AND f.title = 'RT Filter' AND a.action_id = 1",
      )
      .fetch_one(pool)
      .await
      .expect("action count");
      assert_eq!(n_actions, 2, "original + imported action");
          Ok(())
      })).await.unwrap();
}
