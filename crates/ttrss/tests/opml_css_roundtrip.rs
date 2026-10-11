//! OPML round-trip of a multi-line `USER_STYLESHEET` (custom CSS) — the
//! "formatting lost on import" regression (TODO.txt item).
//!
//! PHP's export writes pref values with `htmlspecialchars` only: entities
//! escaped, literal newlines kept inside the attribute. The reference's
//! *import* (DOMDocument) applies XML attribute normalization — newlines
//! squashed to spaces, entities decoded — so a PHP→PHP round trip loses
//! the formatting. This port decodes entities but **keeps the newlines**
//! (deliberate favorable deviation — see `take_attrs` in `opml.rs`), and
//! the export re-escapes the decoded value, so round trips are stable.

mod common;

use test_db::template::with_test_db;
use ttrss::session::Sess;

fn sess_for(uid: i64) -> Sess {
    let mut data = ttrss::core::session::Session::new();
    data.set("uid", ttrss::core::phpser::Value::Int(uid));
    data.set("name", ttrss::core::phpser::Value::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess
}

/// Fetch the stored profile-less USER_STYLESHEET for `uid`.
async fn stored_css(state: &ttrss::AppState, uid: i64) -> String {
    sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 WHERE pref_name='USER_STYLESHEET' AND owner_uid = $1 AND profile IS NULL",
    )
    .bind(uid)
    .fetch_one(state.db.pool())
    .await
    .expect("stored css")
}

#[tokio::test]
async fn test_import_preserves_newlines_and_decodes_entities() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);

            // Exactly the shape of PHP's `opml_export`: htmlspecialchars'd
            // value, literal newlines inside the attribute.
            let opml = "\
<opml version=\"1.0\"><body>
  <outline text=\"tt-rss-prefs\" schema-version=\"151\">
    <outline pref-name=\"USER_STYLESHEET\" value=\"body {
    color: #333;
}
a &gt; b {
    color: red; /* &quot;quoted&quot; */
}\"/>
  </outline>
</body></opml>";
            let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

            let css = stored_css(&state, 1).await;
            // Entities decoded (the PHP-parity part of the fix)…
            assert!(css.contains("a > b"), "entity not decoded:\n{css}");
            assert!(
                css.contains("/* \"quoted\" */"),
                "entity not decoded:\n{css}"
            );
            assert!(!css.contains("&gt;"), "raw entity stored:\n{css}");
            // …and the newlines survive (the favorable deviation).
            assert!(
                css.contains("body {\n    color: #333;\n}"),
                "newlines squashed:\n{css}"
            );
            assert_eq!(css.matches('\n').count(), 5, "newline count:\n{css}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_export_escaping_is_round_trip_stable() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = common::make_app_state(&url).await;
            let sess = sess_for(1);

            let opml = "<opml version=\"1.0\"><body>\
              <outline text=\"tt-rss-prefs\" schema-version=\"151\">\
              <outline pref-name=\"USER_STYLESHEET\" value=\"a &gt; b { color: red; }\"/>\
              </outline></body></opml>";
            let _ = ttrss::opml::import(&state, &sess, Some(opml.as_bytes())).await;

            // Export: the stored (decoded) value re-escaped once — `&gt;`,
            // not the old double-escaped `&amp;gt;`.
            let exported = ttrss::opml::build_opml(state.db.pool(), 1, true).await;
            assert!(
                exported.contains("a &gt; b"),
                "export escaping:\n{exported}"
            );
            assert!(
                !exported.contains("&amp;gt;"),
                "double-escaped export:\n{exported}"
            );

            // Re-import the export: byte-identical stored value.
            let before = stored_css(&state, 1).await;
            let _ = ttrss::opml::import(&state, &sess, Some(exported.as_bytes())).await;
            let after = stored_css(&state, 1).await;
            assert_eq!(before, after, "round trip not stable");
            assert_eq!(after, "a > b { color: red; }");
            Ok(())
        })
    })
    .await
    .unwrap();
}
