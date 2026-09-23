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

mod common;

use serde_json::{json, Map, Value};
use ttrss::api;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

/// A test-only config (the handlers only need `db` for these cases).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        db_max_connections: 10,
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        auth_auto_create: true,
        auth_remote_post_logout_url: String::new(),
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        max_cache_file_size: 64 * 1024 * 1024,
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
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of all fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
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
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getversion", &request(&[]), None).await;
    assert_eq!(status, 0);
    assert_eq!(content["version"], "26.08-2f7d831f");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_api_level() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getapilevel", &request(&[]), None).await;
    assert_eq!(status, 0);
    assert_eq!(content["level"], 23);
        Ok(())
    })).await.unwrap();
}

/// Without the `ENABLE_API_ACCESS` pref (default `false`) the gate rejects.
#[tokio::test]
async fn test_api_disabled() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getversion", &request(&[]), None).await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "API_DISABLED");
        Ok(())
    })).await.unwrap();
}

/// `logout` destroys the session and reports OK; the stored `uid` is cleared
/// (the middleware deletes the stored row on `destroy`).
#[tokio::test]
async fn test_logout() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;
    assert_eq!(sess.data().uid(), Some(1));

    let (status, content) = api::before_and_dispatch(&state, &sess, "logout", &request(&[]), None).await;
    assert_eq!(status, 0);
    assert_eq!(content["status"], "OK");
    assert_eq!(sess.data().uid(), None, "session should be destroyed");
        Ok(())
    })).await.unwrap();
}

/// `logout` is callable even when not logged in (`UserHelper::logout` always
/// succeeds), so it is exempt from the not-logged-in gate.
#[tokio::test]
async fn test_logout_not_logged_in() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, _sess) = setup(&url).await;
    let anon = Sess::new("anon-sid".to_string()); // no `uid`

    let (status, content) = api::before_and_dispatch(&state, &anon, "logout", &request(&[]), None).await;
    assert_eq!(status, 0);
    assert_eq!(content["status"], "OK");
        Ok(())
    })).await.unwrap();
}

/// `catchupFeed` marks a feed's unread entries read and returns `status: OK`.
#[tokio::test]
async fn test_catchup_feed_ok() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 0 (`marked`), mode 1 (`true`): mark the entries.
#[tokio::test]
async fn test_update_marked() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 2 (`unread`), mode 0 (`false`): mark read + set `last_read`.
#[tokio::test]
async fn test_update_unread_mode0() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 0 (`marked`), mode 2 (`toggle`): flip each entry.
#[tokio::test]
async fn test_update_toggle() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 3 (`note`): store the note text.
#[tokio::test]
async fn test_update_note() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 4 (`score`): store the score (rejects `0`).
#[tokio::test]
async fn test_update_score() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` with no `article_ids` → `INCORRECT_USAGE`.
#[tokio::test]
async fn test_update_incorrect_usage_empty() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `updateArticle` field 0 (`marked`) with a non-0/1/2 mode → `INCORRECT_USAGE`.
#[tokio::test]
async fn test_update_incorrect_usage_badmode() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `unsubscribeFeed` removes an owned feed (or `E_OPERATION_FAILED` if absent).
#[tokio::test]
async fn test_unsubscribe_ok() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `setArticleLabel` assign=true attaches label 1 (`label_id = 1026`).
#[tokio::test]
async fn test_set_label_assign() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `setArticleLabel` assign=false removes label 1 from the entry.
#[tokio::test]
async fn test_set_label_remove() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// `setArticleLabel` with an unknown label id → no rows updated.
#[tokio::test]
async fn test_set_label_missing() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

#[tokio::test]
async fn test_share_to_published_ok() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    let ts: Option<String> =
        sqlx::query_scalar("SELECT tsvector_combined::text FROM ttrss_entries WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .expect("ts query");
    assert!(
        ts.expect("tsvector set").contains("world"),
        "tsvector holds the text"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_share_to_published_invalid_url() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

#[tokio::test]
async fn test_share_to_published_no_title_or_url() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

#[tokio::test]
async fn test_share_to_published_updates_existing() {
    let _ = with_test_db(|url| Box::pin(async move {
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
    })).await.unwrap();
}

/// A unique temp dir for feed icons, recreated clean. (M6 normally populates
/// `feed_icon_dir` with fetched favicons; here we write the icon file directly
/// to exercise `getFeedIcon`.)
fn icon_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ttrss-feed-icon-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create icon dir");
    dir
}

/// A uid-1 `AppState` + `Sess` whose `feed_icon_dir` points at `dir`.
async fn setup_icon(url: &str, dir: std::path::PathBuf) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let mut config = test_config();
    config.feed_icon_dir = dir;
    let state = AppState::new(config, db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("icon-sid".to_string());
    sess.replace("icon-sid".to_string(), data);
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
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, _) = setup(&url).await;
    let sess = Sess::new("anon".to_string());
    sess.replace("anon".to_string(), Session::new());
    let body = request(&[("id", json!(55))]);
    let res = api::get_feed_icon(&state, &sess, &body, 1).await;
    let env: Value = serde_json::from_slice(&response_bytes(res).await).unwrap();
    assert_eq!(env["status"], 1);
    assert_eq!(env["content"]["error"], "NOT_LOGGED_IN");
        Ok(())
    })).await.unwrap();
}

/// `getFeedIcon` for a feed with no cached icon → the `E_NOT_FOUND` envelope.
#[tokio::test]
async fn test_get_feed_icon_not_found() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    enable_api(&state.db).await;
    let body = request(&[("id", json!(55))]);
    let res = api::get_feed_icon(&state, &sess, &body, 1).await;
    let env: Value = serde_json::from_slice(&response_bytes(res).await).unwrap();
    assert_eq!(env["status"], 1);
    assert_eq!(env["content"]["error"], "E_NOT_FOUND");
        Ok(())
    })).await.unwrap();
}

/// `getFeedIcon` for a feed with a cached PNG icon → 200 + `image/png` + bytes.
#[tokio::test]
async fn test_get_feed_icon_found_png() {
    let _ = with_test_db(|url| Box::pin(async move {
    let dir = icon_dir("png");
    let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
    std::fs::write(dir.join("55"), &png).expect("write icon");
    let (state, sess) = setup_icon(&url, dir).await;
    enable_api(&state.db).await;
    let body = request(&[("id", json!(55))]);
    let res = api::get_feed_icon(&state, &sess, &body, 1).await;
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    assert_eq!(response_bytes(res).await, png);
        Ok(())
    })).await.unwrap();
}

/// `getFeedIcon` content-type detection: a JPEG icon → `image/jpeg`.
#[tokio::test]
async fn test_get_feed_icon_found_jpeg() {
    let _ = with_test_db(|url| Box::pin(async move {
    let dir = icon_dir("jpeg");
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
    std::fs::write(dir.join("7"), &jpeg).expect("write icon");
    let (state, sess) = setup_icon(&url, dir).await;
    enable_api(&state.db).await;
    let body = request(&[("id", json!(7))]);
    let res = api::get_feed_icon(&state, &sess, &body, 1).await;
    assert_eq!(
        res.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
        "image/jpeg"
    );
    assert_eq!(response_bytes(res).await, jpeg);
        Ok(())
    })).await.unwrap();
}
