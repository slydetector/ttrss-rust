//! DB tests for the OPML **import** (M4: feed CRUD, OPML section).
//!
//! Rust-only: the frozen PHP suite has no OPML import tests, so these exercise
//! the ported `opml::import` directly on the versioned `schema` + `baseline` +
//! `seed` fixtures. The bar is the resulting DB state: categories created (and
//! deduped by title + parent), feeds inserted (deduped by `feed_url` + owner),
//! and the `tt-rss-prefs` / `tt-rss-labels` / `tt-rss-filters` settings sections
//! applied.

use crate::common;

use sqlx::AssertSqlSafe;
use test_db::template::with_test_db;
use ttrss::session::Sess;

/// A logged-in session for `uid` (mirrors the `opml_export.rs` setup).
fn sess_for(uid: i32) -> Sess {
    let mut data = ttrss::core::session::Session::new();
    data.set("uid", ttrss::core::phpser::Value::Int(uid as i64));
    data.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess
}

async fn count(pool: &sqlx::PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .expect("count query")
}

#[tokio::test]
async fn test_import_creates_category_and_feed() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
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
    let body = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    // The response body is the `opml_notice` lines (3×`&nbsp;` per depth level).
    assert!(
        body.contains("Processing category: Uncategorized<br/>"),
        "root notice: {body}"
    );
    assert!(
        body.contains("&nbsp;&nbsp;&nbsp;Processing category: New Cat<br/>"),
        "category notice (1 level): {body}"
    );
    assert!(
        body.contains("&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;Adding feed: New Feed<br/>"),
        "feed notice (2 levels): {body}"
    );

    // The category is created.
    let cat: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = 'New Cat'",
    )
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_lenient_int_attrs() {
    // PHP casts the ttrss* attributes with (int) — a lenient
    // leading-numeric-prefix parse — not a strict parse. Non-integer
    // attribute values must import their prefix, not 0.
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="Len Cat" ttrssSortOrder=" 7x">
        <outline type="rss" text="Len Feed" xmlUrl="https://example.com/len" ttrssUpdateInterval="3.14" ttrssSortOrder=" 5" ttrssPurgeInterval="5abc"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

    // Category order_id: (int)" 7x" = 7.
    let cat: Option<i32> = sqlx::query_scalar(
        "SELECT order_id FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = 'Len Cat'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("cat query");
    assert_eq!(cat, Some(7), "category ttrssSortOrder (int) cast");

    // Feed: (int)"3.14" = 3, (int)" 5" = 5, (int)"5abc" = 5.
    let row: Option<(i32, i32, i32)> = sqlx::query_as(
        "SELECT order_id, update_interval, purge_interval FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/len'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("feed query");
    let (forder, fupdate, fpurge) = row.expect("Len Feed created");
    assert_eq!(fupdate, 3, "ttrssUpdateInterval (int) cast");
    assert_eq!(forder, 5, "ttrssSortOrder (int) cast");
    assert_eq!(fpurge, 5, "ttrssPurgeInterval (int) cast");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_dedupes_feeds() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="Dup Cat">
        <outline type="rss" text="Dup Feed" xmlUrl="https://example.com/dup"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let first = count(state.db.pool(), "SELECT count(*) FROM ttrss_feeds WHERE feed_url = 'https://example.com/dup' AND owner_uid = 1").await;
    assert_eq!(first, 1, "first import creates one feed");

    // Import again: the feed (by url) and the category (by title) are both deduped.
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let second = count(state.db.pool(), "SELECT count(*) FROM ttrss_feeds WHERE feed_url = 'https://example.com/dup' AND owner_uid = 1").await;
    let cats = count(
        state.db.pool(),
        "SELECT count(*) FROM ttrss_feed_categories WHERE title = 'Dup Cat' AND owner_uid = 1",
    )
    .await;
    assert_eq!(second, 1, "no duplicate feed");
    assert_eq!(cats, 1, "no duplicate category");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_top_level_feed_uncategorized() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    // A top-level feed (not inside a category) goes to the "Imported feeds"
    // category if it exists, else NULL (uncategorized). No such category here.
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" text="Top Feed" xmlUrl="https://example.com/top"/>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let cat: Option<Option<i32>> = sqlx::query_scalar::<_, Option<i32>>("SELECT cat_id FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/top'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(
        cat,
        Some(None),
        "top-level feed is uncategorized when no 'Imported feeds' cat"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_top_level_feed_uses_imported_cat() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    // Pre-create the "Imported feeds" category: a top-level feed now lands in it.
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (owner_uid, title) VALUES (1, 'Imported feeds')",
    )
    .execute(state.db.pool())
    .await
    .expect("insert Imported feeds");
    let imported_cat: i32 = sqlx::query_scalar(
        "SELECT id FROM ttrss_feed_categories WHERE owner_uid = 1 AND title = 'Imported feeds'",
    )
    .fetch_one(state.db.pool())
    .await
    .expect("cat id");
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" text="Top Feed" xmlUrl="https://example.com/top"/>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let cat: Option<Option<i32>> = sqlx::query_scalar::<_, Option<i32>>("SELECT cat_id FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/top'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(
        cat,
        Some(Some(imported_cat)),
        "top-level feed lands in 'Imported feeds'"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_unknown_feed_title() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    // No text/title → the feed title defaults to "[Unknown]".
    let opml = r#"<opml version="1.0"><body>
      <outline type="rss" xmlUrl="https://example.com/untitled"/>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE owner_uid = 1 AND feed_url = 'https://example.com/untitled'")
        .fetch_optional(state.db.pool())
        .await
        .expect("query");
    assert_eq!(
        title,
        Some("[Unknown]".to_string()),
        "untitled feed defaults to [Unknown]"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_parse_error_returns_error() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);
            let body = ttrss::opml::import(&state, &sess, Some(b"this is not xml <<<")).await;
            assert!(
                body.contains("Error while parsing document.<br/>"),
                "parse error reported: {body}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_import_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);
            let opml = r##"<opml version="1.0"><body>
      <outline text="tt-rss-labels">
        <outline label-name="My Label" label-fg-color="#ff0000" label-bg-color="#00ff00"/>
        <outline label-name="Second Label"/>
      </outline>
    </body></opml>"##;
            let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

            let row: Option<(String, String)> = sqlx::query_as(
        "SELECT fg_color, bg_color FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'My Label'",
    )
    .fetch_optional(state.db.pool())
    .await
    .expect("label query");
            let (fg, bg) = row.expect("My Label created");
            assert_eq!(fg, "#ff0000");
            assert_eq!(bg, "#00ff00");
            let second = count(
        state.db.pool(),
        "SELECT count(*) FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'Second Label'",
    )
    .await;
            assert_eq!(second, 1, "Second Label created (default colors)");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_import_labels_dedup() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);
            let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-labels">
        <outline label-name="Dup Label"/>
      </outline>
    </body></opml>"#;
            let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
            let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;
            let n = count(
                state.db.pool(),
                "SELECT count(*) FROM ttrss_labels2 WHERE owner_uid = 1 AND caption = 'Dup Label'",
            )
            .await;
            assert_eq!(n, 1, "label deduped by caption");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_import_prefs() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-prefs">
        <outline pref-name="PURGE_OLD_DAYS" value="45"/>
        <outline pref-name="SHORT_DATE_FORMAT" value="Y-m-d"/>
      </outline>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_filter() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-filters">
        <outline filter-type="2"><![CDATA[{"match_any_rule":false,"enabled":true,"inverse":false,"title":"My Filter","rules":[{"reg_exp":"spam","filter_type":1,"cat_filter":false,"feed":"Hacker News","inverse":false}],"actions":[{"action_id":1,"action_param":""}]}]]></outline>
      </outline>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

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

    let action: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2_actions WHERE filter_id = $1 AND action_id = 1",
    )
    .bind(filter_id)
    .fetch_one(state.db.pool())
    .await
    .expect("action query");
    assert_eq!(action, 1, "action created");
        Ok(())
    })).await.unwrap();
}

/// PHP `(int)$action["action_id"]` casts a missing key (null) to 0; the
/// `action_id` FK (`ttrss_filter_actions`, no id-0 row) then rejects the
/// insert — the port swallows the error (like its other import inserts) and
/// continues. The filter itself must still be created.
#[tokio::test]
async fn test_import_filter_action_missing_id_rejected_by_fk() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let sess = sess_for(1);
    let opml = r#"<opml version="1.0"><body>
      <outline text="tt-rss-filters">
        <outline filter-type="2"><![CDATA[{"match_any_rule":false,"enabled":true,"inverse":false,"title":"NoId Filter","rules":[],"actions":[{"action_param":"5"}]}]]></outline>
      </outline>
    </body></opml>"#;
    let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

    let filter: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2 WHERE title = 'NoId Filter'",
    )
    .fetch_one(state.db.pool())
    .await
    .expect("filter query");
    assert_eq!(filter, 1, "filter created");

    let actions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2_actions a \
         JOIN ttrss_filters2 f ON f.id = a.filter_id WHERE f.title = 'NoId Filter'",
    )
    .fetch_one(state.db.pool())
    .await
    .expect("action query");
    assert_eq!(actions, 0, "action_id 0 fails the FK (PHP parity)");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_import_no_file_reports_upload_error() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);
            let body = ttrss::opml::import(&state, &sess, None).await;
            // PHP: `$_FILES['opml_file']['error']` = UPLOAD_ERR_NO_FILE (4) → `print_error`.
            assert!(
                body.contains("Upload failed with error code 4"),
                "no-file error: {body}"
            );
            assert!(body.contains("alert alert-danger"), "error div: {body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A full session (uid + access level + stored pwd hash + CSRF token), so
/// `validate_session` + the CSRF gate pass and the routing is what's under test.
fn sess_authed(uid: i32) -> Sess {
    let mut d = ttrss::core::session::Session::new();
    d.set("uid", ttrss::core::phpser::Value::Int(uid as i64));
    d.set(
        "access_level",
        ttrss::core::phpser::Value::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64),
    );
    d.set(
        "pwd_hash",
        ttrss::core::phpser::Value::Bytes(
            b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec(),
        ),
    );
    d.set(
        "auth_module",
        ttrss::core::phpser::Value::Bytes(b"auth_internal".to_vec()),
    );
    d.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
    d.set(
        "csrf_token",
        ttrss::core::phpser::Value::Bytes(b"tok".to_vec()),
    );
    let s = Sess::new("test-sid".to_string());
    s.replace("test-sid".to_string(), d);
    s
}

/// The UI's OPML import dialog posts the `opml_import_form` (multipart, field
/// `opml_file`) to `backend.php` with `op=Pref_Feeds&method=importOpml`. This
/// drives the full handler to prove the route exists (regression: it used to
/// 404 as `E_UNKNOWN_METHOD`) and returns the `opml_notice` body.
#[tokio::test]
async fn test_dispatch_pref_feeds_importopml() {
    with_test_db(|url| Box::pin(async move {
    let state = common::make_app_state(&url).await;
    let opml = r#"<?xml version="1.0" encoding="utf-8"?>
<opml version="1.0">
  <body>
    <outline type="rss" text="Imported via RPC" xmlUrl="https://example.com/rpc" htmlUrl="https://example.com"/>
  </body>
</opml>"#;

    let boundary = "----testboundary123";
    let mut body = String::new();
    for (name, value) in [("op", "Pref_Feeds"), ("method", "importOpml"), ("csrf_token", "tok")] {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"opml_file\"; filename=\"feeds.opml\"\r\nContent-Type: application/octet-stream\r\n\r\n{opml}\r\n"
    ));
    body.push_str(&format!("--{boundary}--\r\n"));

    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/backend.php")
        .header(
            axum::http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))
        .unwrap();
    let res = ttrss::rpc::handler(
        axum::extract::State(state.clone()),
        axum::extract::Extension(sess_authed(1)),
        req,
    )
    .await;
    let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("Adding feed: Imported via RPC"),
        "importopml routed + feed notice: {text}"
    );
    assert!(
        text.contains("Processing category: Uncategorized<br/>"),
        "root notice: {text}"
    );

    // The feed actually landed (top-level → `Imported feeds` cat).
    let n: i64 = count(
        state.db.pool(),
        "SELECT COUNT(*) FROM ttrss_feeds WHERE feed_url = 'https://example.com/rpc'",
    )
    .await;
    assert_eq!(n, 1, "feed inserted");
        Ok(())
    })).await.unwrap();
}
