//! DB tests for the M5 external-API write methods: `getVersion`, `getApiLevel`,
//! `catchupFeed`, `unsubscribeFeed`, `updateArticle`, `setArticleLabel`.
//!
//! Rust-only: the frozen PHP suite's `tests/integration/ApiTest.php` only covers
//! `login`/`getVersion`/`getUnread`/`subscribeToFeed`/`getCounters`/`getFeedTree`/
//! `getHeadlines` (most already ported; `subscribeToFeed` is M6). These exercise
//! the ported write handlers directly against the versioned `schema` +
//! `baseline` + `seed` fixtures (feeds 1-4, labels 1-2, entries 1-11 for owner 1).
//!
//! The handlers are called two ways: directly (their own logic) and, for the
//! auth gate + meta methods, through `before_and_dispatch` (the op routing).

use crate::common;

use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::api;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;

/// A test-only config (the handlers only need `db` for these cases).
/// Fresh fixtures + a session carrying `uid = 1` (owner of all fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

/// `$_REQUEST` payload as the handler's body map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Enable the per-user API gate (the `before_and_dispatch` auth check).
async fn enable_api(db: &Db) {
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
}

#[tokio::test]
async fn test_get_version() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            enable_api(&state.db).await;
            let (status, content) =
                api::before_and_dispatch(&state, &sess, "getversion", &request(&[]), None).await;
            assert_eq!(status, 0);
            assert_eq!(content["version"], "27.10-270fcc71");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_get_api_level() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            enable_api(&state.db).await;
            let (status, content) =
                api::before_and_dispatch(&state, &sess, "getapilevel", &request(&[]), None).await;
            assert_eq!(status, 0);
            assert_eq!(content["level"], 23);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Without the `ENABLE_API_ACCESS` pref (default `false`) the gate rejects.
#[tokio::test]
async fn test_api_disabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) =
                api::before_and_dispatch(&state, &sess, "getversion", &request(&[]), None).await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "API_DISABLED");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `logout` destroys the session and reports OK; the stored `uid` is cleared
/// (the middleware deletes the stored row on `destroy`).
#[tokio::test]
async fn test_logout() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            enable_api(&state.db).await;
            assert_eq!(sess.data().uid(), Some(1));

            let (status, content) =
                api::before_and_dispatch(&state, &sess, "logout", &request(&[]), None).await;
            assert_eq!(status, 0);
            assert_eq!(content["status"], "OK");
            assert_eq!(sess.data().uid(), None, "session should be destroyed");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `logout` is callable even when not logged in (`UserHelper::logout` always
/// succeeds), so it is exempt from the not-logged-in gate.
#[tokio::test]
async fn test_logout_not_logged_in() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = setup(&url).await;
            let anon = Sess::new("anon-sid".to_string()); // no `uid`

            let (status, content) =
                api::before_and_dispatch(&state, &anon, "logout", &request(&[]), None).await;
            assert_eq!(status, 0);
            assert_eq!(content["status"], "OK");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `catchupFeed` marks a feed's unread entries read and returns `status: OK`.
#[tokio::test]
async fn test_catchup_feed_ok() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            // refs 1,2 are feed 1 and unread; ref 3 is feed 1 and already read.
            let (status, content) =
                api::catchup_feed(&state, &sess, &request(&[("feed_id", json!("1"))])).await;
            assert_eq!(status, 0);
            assert_eq!(content["status"], "OK");
            let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE feed_id = 1 AND unread AND owner_uid = 1",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
            assert_eq!(unread, 0, "feed 1 still has unread entries");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 0 (`marked`), mode 1 (`true`): mark the entries.
#[tokio::test]
async fn test_update_marked() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1, 2])),
                    ("mode", json!(1)),
                    ("field", json!(0)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 2);
            for r in [1i32, 2] {
                let marked: bool = sqlx::query_scalar(
                    "SELECT marked FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = 1",
                )
                .bind(r)
                .fetch_one(state.db.pool())
                .await
                .unwrap();
                assert!(marked, "ref {r} not marked");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 2 (`unread`), mode 0 (`false`): mark read + set `last_read`.
#[tokio::test]
async fn test_update_unread_mode0() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1, 4])),
                    ("mode", json!(0)),
                    ("field", json!(2)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 2);
            for r in [1i32, 4] {
                let (unread, last_read_set): (bool, bool) = sqlx::query_as(
                    "SELECT unread, (last_read IS NOT NULL) FROM ttrss_user_entries \
             WHERE ref_id = $1 AND owner_uid = 1",
                )
                .bind(r)
                .fetch_one(state.db.pool())
                .await
                .unwrap();
                assert!(!unread && last_read_set, "ref {r} not marked read");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 0 (`marked`), mode 2 (`toggle`): flip each entry.
#[tokio::test]
async fn test_update_toggle() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            // Seed refs 1,2,4 are unmarked; pre-mark ref 4 so the toggle flips it back.
            sqlx::query("UPDATE ttrss_user_entries SET marked = true WHERE ref_id = 4")
                .execute(state.db.pool())
                .await
                .unwrap();
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1, 4])),
                    ("mode", json!(2)),
                    ("field", json!(0)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 2);
            let m1: bool = sqlx::query_scalar(
                "SELECT marked FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            let m4: bool = sqlx::query_scalar(
                "SELECT marked FROM ttrss_user_entries WHERE ref_id = 4 AND owner_uid = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert!(m1, "ref 1 (was unmarked) should now be marked");
            assert!(!m4, "ref 4 (was marked) should now be unmarked");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 3 (`note`): store the note text.
#[tokio::test]
async fn test_update_note() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1])),
                    ("field", json!(3)),
                    ("data", json!("hello & goodbye")), // `clean()` strips tags; no tags here
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 1);
            let note: Option<String> = sqlx::query_scalar(
                "SELECT note FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(note.as_deref(), Some("hello & goodbye"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 4 (`score`): store the score (rejects `0`).
#[tokio::test]
async fn test_update_score() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1, 2])),
                    ("field", json!(4)),
                    ("data", json!("5")),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 2);
            for r in [1i32, 2] {
                let score: i32 = sqlx::query_scalar(
                    "SELECT score FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = 1",
                )
                .bind(r)
                .fetch_one(state.db.pool())
                .await
                .unwrap();
                assert_eq!(score, 5, "ref {r} score != 5");
            }
            // `score` 0 is falsy (`if ($field && $set_to)`) → `INCORRECT_USAGE`.
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1])),
                    ("field", json!(4)),
                    ("data", json!("0")),
                ]),
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "INCORRECT_USAGE");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` with no `article_ids` → `INCORRECT_USAGE`.
#[tokio::test]
async fn test_update_incorrect_usage_empty() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[("field", json!(0)), ("mode", json!(1))]),
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "INCORRECT_USAGE");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `updateArticle` field 0 (`marked`) with a non-0/1/2 mode → `INCORRECT_USAGE`.
#[tokio::test]
async fn test_update_incorrect_usage_badmode() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::update_article(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1])),
                    ("field", json!(0)),
                    ("mode", json!(3)),
                ]),
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "INCORRECT_USAGE");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `unsubscribeFeed` removes an owned feed (or `E_OPERATION_FAILED` if absent).
#[tokio::test]
async fn test_unsubscribe_ok() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) =
                api::unsubscribe_feed(&state, &sess, &request(&[("feed_id", json!(1))])).await;
            assert_eq!(status, 0);
            assert_eq!(content["status"], "OK");
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE id = 1")
                .fetch_one(state.db.pool())
                .await
                .unwrap();
            assert_eq!(count, 0, "feed 1 not removed");
            // Missing feed → `E_OPERATION_FAILED`.
            let (status, content) =
                api::unsubscribe_feed(&state, &sess, &request(&[("feed_id", json!(999))])).await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "E_OPERATION_FAILED");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `setArticleLabel` assign=true attaches label 1 (`label_id = 1026`).
#[tokio::test]
async fn test_set_label_assign() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::set_article_label(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1, 2])),
                    ("label_id", json!(1026)),
                    ("assign", json!(true)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 2);
            let labeled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_labels2 WHERE label_id = 1 AND article_id IN (1, 2)",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
            assert_eq!(labeled, 2, "refs 1,2 not labeled");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `setArticleLabel` assign=false removes label 1 from the entry.
#[tokio::test]
async fn test_set_label_remove() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            // Pre-attach label 1 to ref 1.
            sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 1)")
                .execute(state.db.pool())
                .await
                .unwrap();
            let (status, content) = api::set_article_label(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1])),
                    ("label_id", json!(1026)),
                    ("assign", json!(false)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 1);
            let labeled: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_user_labels2 WHERE label_id = 1 AND article_id = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(labeled, 0, "ref 1 still labeled");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `setArticleLabel` with an unknown label id → no rows updated.
#[tokio::test]
async fn test_set_label_missing() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::set_article_label(
                &state,
                &sess,
                &request(&[
                    ("article_ids", json!([1])),
                    ("label_id", json!(99999)),
                    ("assign", json!(true)),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["updated"], 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_share_to_published_ok() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let pool = state.db.pool();
            let (status, content) = api::share_to_published(
                &state,
                &sess,
                &request(&[
                    ("title", json!("My shared post")),
                    ("url", json!("https://example.com/article-1")),
                    ("content", json!("<p>Hello <b>world</b></p>")),
                ]),
            )
            .await;
            assert_eq!(status, 0);
            assert_eq!(content["status"], "OK");

            // A new entry + a published user entry were created.
            let row: Option<(i32, String, bool)> = sqlx::query_as(
                "SELECT e.id, e.guid, ue.published FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id AND ue.owner_uid = 1 \
         WHERE e.link = 'https://example.com/article-1'",
            )
            .fetch_optional(pool)
            .await
            .expect("entry query");
            let (id, guid, published) = row.expect("entry created");
            assert!(guid.starts_with("SHA1:"), "guid is SHA1:");
            assert!(published, "the shared article is marked published");

            // The tsvector was populated (searchable text).
            let ts: Option<String> = sqlx::query_scalar(
                "SELECT tsvector_combined::text FROM ttrss_entries WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .expect("ts query");
            assert!(
                ts.expect("tsvector set").contains("world"),
                "tsvector holds the text"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_share_to_published_invalid_url() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::share_to_published(
                &state,
                &sess,
                &request(&[
                    ("title", json!("Nope")),
                    ("url", json!("not a url")),
                    ("content", json!("body")),
                ]),
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "E_OPERATION_FAILED");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_share_to_published_no_title_or_url() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let (status, content) = api::share_to_published(
                &state,
                &sess,
                &request(&[
                    ("title", json!("")),
                    ("url", json!("")),
                    ("content", json!("body")),
                ]),
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(content["error"], "E_OPERATION_FAILED");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_share_to_published_updates_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let pool = state.db.pool();

            // Share the same URL twice (different content).
            for content in ["<p>first</p>", "<p>second body</p>"] {
                let (status, _) = api::share_to_published(
                    &state,
                    &sess,
                    &request(&[
                        ("title", json!("Post")),
                        ("url", json!("https://example.com/same")),
                        ("content", json!(content)),
                    ]),
                )
                .await;
                assert_eq!(status, 0);
            }

            // Exactly one entry + one user entry (the second share updates, not
            // duplicates).
            let entry_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_entries WHERE link = 'https://example.com/same'",
            )
            .fetch_one(pool)
            .await
            .expect("count");
            assert_eq!(entry_count, 1, "no duplicate entry");
            let ue_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_user_entries ue \
         JOIN ttrss_entries e ON e.id = ue.ref_id \
         WHERE e.link = 'https://example.com/same' AND ue.owner_uid = 1",
            )
            .fetch_one(pool)
            .await
            .expect("ue count");
            assert_eq!(ue_count, 1, "no duplicate user entry");
            // The content was updated to the second share (cleaned: tags stripped).
            let stored: String = sqlx::query_scalar(
                "SELECT content FROM ttrss_entries WHERE link = 'https://example.com/same'",
            )
            .fetch_one(pool)
            .await
            .expect("content");
            assert_eq!(
                stored, "second body",
                "content is clean()ed (strip_tags + trim)"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A unique temp dir for feed icons, recreated clean. (M6 normally populates
/// `feed_icon_dir` with fetched favicons; here we write the icon file directly
/// to exercise `getFeedIcon`.)
fn icon_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ttrss-feed-icon-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("feed-icons")).expect("create icon dir");
    dir
}

/// A uid-1 `AppState` + `Sess` whose `cache_dir` is `dir` (icons at `dir/feed-icons`).
async fn setup_icon(url: &str, dir: std::path::PathBuf) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let mut config = common::test_config();
    config.cache_dir = dir;
    let state = AppState::new(config, db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("icon-sid".to_string());
    sess.replace("icon-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

/// Read an axum `Response` body to bytes (for the raw / JSON response).
async fn response_bytes(res: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(res.into_body(), 4096)
        .await
        .expect("read body")
        .to_vec()
}

/// `getFeedIcon` with no session → the `NOT_LOGGED_IN` JSON envelope.
#[tokio::test]
async fn test_get_feed_icon_not_logged_in() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _) = setup(&url).await;
            let sess = Sess::new("anon".to_string());
            sess.replace("anon".to_string(), Session::new());
            let body = request(&[("id", json!(55))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            let env: Value = serde_json::from_slice(&response_bytes(res).await).unwrap();
            assert_eq!(env["status"], 1);
            assert_eq!(env["content"]["error"], "NOT_LOGGED_IN");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `getFeedIcon` for a feed with no cached icon → the `E_NOT_FOUND` envelope.
#[tokio::test]
async fn test_get_feed_icon_not_found() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(55))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            let env: Value = serde_json::from_slice(&response_bytes(res).await).unwrap();
            assert_eq!(env["status"], 1);
            assert_eq!(env["content"]["error"], "E_NOT_FOUND");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `getFeedIcon` for a feed with a cached PNG icon → 200 + `image/png` + bytes.
#[tokio::test]
async fn test_get_feed_icon_found_png() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("png");
            let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
            std::fs::write(dir.join("feed-icons").join("55"), &png).expect("write icon");
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(55))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            assert_eq!(
                res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
                "image/png"
            );
            assert_eq!(response_bytes(res).await, png);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `getFeedIcon` content-type detection: a JPEG icon → `image/jpeg`.
#[tokio::test]
async fn test_get_feed_icon_found_jpeg() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("jpeg");
            let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
            std::fs::write(dir.join("feed-icons").join("7"), &jpeg).expect("write icon");
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(7))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(
                res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
                "image/jpeg"
            );
            assert_eq!(response_bytes(res).await, jpeg);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── getFeedIcon: PHP DiskCache::send parity (conditional requests + gate) ──

const ICON_MTIME: i64 = 1_700_000_000;

/// Write `bytes` to `dir/feed-icons/{id}` and pin its mtime (PHP's ETag /
/// Last-Modified source is the file mtime — a stable value keeps the expected
/// header strings deterministic).
fn write_icon(dir: &std::path::Path, id: i32, bytes: &[u8]) {
    let path = dir.join("feed-icons").join(id.to_string());
    std::fs::write(&path, bytes).expect("write icon");
    std::fs::File::open(&path)
        .expect("open icon")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(ICON_MTIME as u64))
        .expect("pin mtime");
}

/// The `D, d M Y H:i:s GMT` form `api::http_date` emits (gmdate parity).
/// A 200 icon carries the full PHP header set: ETag (bare mtime),
/// Last-Modified, Expires (mtime + CACHE_MAX_DAYS), Cache-Control, and the
/// Content-Disposition fake extension.
#[tokio::test]
async fn test_get_feed_icon_headers() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("headers");
            let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
            write_icon(&dir, 55, &png);
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(55))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            let h = res.headers();
            // `send_file`'s LRU touch refreshes the mtime on access, so the
            // stamps are shape-checked, not pinned to `ICON_MTIME`.
            let etag = h.get(axum::http::header::ETAG).unwrap().to_str().unwrap();
            assert!(
                etag.parse::<i64>().is_ok(),
                "ETag is the bare mtime: {etag}"
            );
            let gmt = |v: &str| -> i64 {
                // `D, d M Y H:i:s GMT` (gmdate's HTTP form) → unix secs.
                chrono::NaiveDateTime::parse_from_str(&v[..v.len() - 4], "%a, %d %b %Y %H:%M:%S")
                    .expect("gmdate form")
                    .and_utc()
                    .timestamp()
            };
            let parsed = gmt(h
                .get(axum::http::header::LAST_MODIFIED)
                .unwrap()
                .to_str()
                .unwrap());
            let parsed_exp = gmt(h
                .get(axum::http::header::EXPIRES)
                .unwrap()
                .to_str()
                .unwrap());
            assert_eq!(
                parsed_exp - parsed,
                86400 * 7,
                "Expires = mtime + TTRSS_CACHE_MAX_DAYS (7)"
            );
            assert_eq!(
                h.get(axum::http::header::CACHE_CONTROL).unwrap(),
                "no-cache"
            );
            assert_eq!(
                h.get(axum::http::header::CONTENT_DISPOSITION).unwrap(),
                "inline; filename=\"55.png\""
            );
            assert_eq!(
                h.get(axum::http::header::CONTENT_TYPE).unwrap(),
                "image/png"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `If-Modified-Since` matching the stamped `Last-Modified` → 304 with an
/// empty body (the 200-equivalent headers are still sent).
#[tokio::test]
async fn test_get_feed_icon_304_if_modified_since() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("ims304");
            write_icon(&dir, 9, b"\x89PNG\r\n\x1a\nbody");
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(9))]);
            // First request: 200 with the stamps the client echoes back
            // (`send_file`'s LRU touch sets the mtime, so read them from the
            // response rather than pinning `ICON_MTIME`).
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            let last_modified = res
                .headers()
                .get(axum::http::header::LAST_MODIFIED)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::IF_MODIFIED_SINCE,
                last_modified.parse().unwrap(),
            );
            let res = api::get_feed_icon(&state, &sess, &body, 2, &headers).await;
            assert_eq!(res.status(), 304);
            let h = res.headers();
            assert!(
                h.get(axum::http::header::ETAG)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .parse::<i64>()
                    .is_ok(),
                "ETag is the bare mtime"
            );
            assert_eq!(
                h.get(axum::http::header::LAST_MODIFIED)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                last_modified
            );
            assert_eq!(
                h.get(axum::http::header::CACHE_CONTROL)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "no-cache"
            );
            assert!(response_bytes(res).await.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `If-None-Match` matching the ETag (the bare mtime string) → 304.
#[tokio::test]
async fn test_get_feed_icon_304_if_none_match() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("inm304");
            write_icon(&dir, 9, b"\x89PNG\r\n\x1a\nbody");
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(9))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            let etag = res
                .headers()
                .get(axum::http::header::ETAG)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(axum::http::header::IF_NONE_MATCH, etag.parse().unwrap());
            let res = api::get_feed_icon(&state, &sess, &body, 2, &headers).await;
            assert_eq!(res.status(), 304);
            assert!(response_bytes(res).await.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// An SVG with a UTF-8 BOM + leading whitespace before `<svg` is still
/// detected as `image/svg+xml` and served (the WHATWG-style trim in
/// `sniff_mime` — without it, the BOM masks `<svg` and the content would
/// remap to `video/mp4`).
#[tokio::test]
async fn test_get_feed_icon_bom_svg() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("bomsvg");
            let svg: Vec<u8> = b"\xEF\xBB\xBF  \n<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"
                .to_vec();
            write_icon(&dir, 9, &svg);
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(9))]);
            let res = api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            assert_eq!(
                res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
                "image/svg+xml"
            );
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CONTENT_DISPOSITION)
                    .unwrap(),
                "inline; filename=\"9.svg\""
            );
            assert_eq!(response_bytes(res).await, svg);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// An XML-prolog SVG (Adobe Illustrator export) is detected via the
/// `<?xml` + `<svg` look-ahead and served with the `.svg` fake extension.
#[tokio::test]
async fn test_get_feed_icon_svg_served() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("svg");
            write_icon(&dir, 3, b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>");
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(3))]);
            let res = api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            assert_eq!(
                res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
                "image/svg+xml"
            );
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CONTENT_DISPOSITION)
                    .unwrap(),
                "inline; filename=\"3.svg\""
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A stored HTML page (a feed host's 404 saved as the favicon) is not
/// image/audio/video → 400.
#[tokio::test]
async fn test_get_feed_icon_html_rejected() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("html");
            write_icon(
                &dir,
                4,
                b"<!DOCTYPE html><html><body>Not Found</body></html>",
            );
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(4))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 400);
            assert_eq!(
                String::from_utf8(response_bytes(res).await).unwrap(),
                "Stored file has disallowed content type (text/html)"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// An ICO favicon (common from Windows-served sites) is detected by `infer`
/// and served as `image/vnd.microsoft.icon` with the `.ico` fake extension
/// (the hand-rolled detector used to remap it to `video/mp4`).
#[tokio::test]
async fn test_get_feed_icon_ico() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("ico");
            let ico: Vec<u8> = vec![
                0, 0, // reserved
                1, 0, // type: icon (u16 LE)
                1, 0, // one image (u16 LE)
                32, 0, 32, 0, // 32x32
                0, 0, 2, 0, // BITMAPINFOHEADER (40 bytes) + pixel data
                40, 0, 0, 0, 32, 0, 64, 0, 1, 0, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ];
            write_icon(&dir, 8, &ico);
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(8))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "image/vnd.microsoft.icon"
            );
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CONTENT_DISPOSITION)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "inline; filename=\"8.ico\""
            );
            assert_eq!(response_bytes(res).await, ico);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Unrecognized binary: finfo's `application/octet-stream` → PHP's `video/mp4`
/// remap (served, with the `.mp4` fake extension).
#[tokio::test]
async fn test_get_feed_icon_unknown_binary_served_mp4() {
    with_test_db(|url| {
        Box::pin(async move {
            let dir = icon_dir("mp4");
            let bin = b"\x00\x01\x02\x03\x04";
            write_icon(&dir, 6, bin);
            let (state, sess) = setup_icon(&url, dir).await;
            enable_api(&state.db).await;
            let body = request(&[("id", json!(6))]);
            let res =
                api::get_feed_icon(&state, &sess, &body, 1, &axum::http::HeaderMap::new()).await;
            assert_eq!(res.status(), 200);
            assert_eq!(
                res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
                "video/mp4"
            );
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CONTENT_DISPOSITION)
                    .unwrap(),
                "inline; filename=\"6.pdf\""
            );
            assert_eq!(response_bytes(res).await, bin);
            Ok(())
        })
    })
    .await
    .unwrap();
}
