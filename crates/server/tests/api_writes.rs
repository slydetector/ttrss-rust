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
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::api;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

/// A test-only config (the handlers only need `db` for these cases).
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

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_version(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    enable_api(&state.db).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getversion", &request(&[])).await;
    assert_eq!(status, 0);
    assert_eq!(content["version"], "26.08-2f7d831f");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_api_level(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    enable_api(&state.db).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getapilevel", &request(&[])).await;
    assert_eq!(status, 0);
    assert_eq!(content["level"], 23);
}

/// Without the `ENABLE_API_ACCESS` pref (default `false`) the gate rejects.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_api_disabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) =
        api::before_and_dispatch(&state, &sess, "getversion", &request(&[])).await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "API_DISABLED");
}

/// `catchupFeed` marks a feed's unread entries read and returns `status: OK`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_catchup_feed_ok(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    // refs 1,2 are feed 1 and unread; ref 3 is feed 1 and already read.
    let (status, content) = api::catchup_feed(&state, &sess, &request(&[("feed_id", json!("1"))])).await;
    assert_eq!(status, 0);
    assert_eq!(content["status"], "OK");
    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE feed_id = 1 AND unread AND owner_uid = 1",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    assert_eq!(unread, 0, "feed 1 still has unread entries");
}

/// `updateArticle` field 0 (`marked`), mode 1 (`true`): mark the entries.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_marked(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1, 2])),
        ("mode", json!(1)),
        ("field", json!(0)),
    ]))
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
}

/// `updateArticle` field 2 (`unread`), mode 0 (`false`): mark read + set `last_read`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_unread_mode0(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1, 4])),
        ("mode", json!(0)),
        ("field", json!(2)),
    ]))
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
}

/// `updateArticle` field 0 (`marked`), mode 2 (`toggle`): flip each entry.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_toggle(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    // Seed refs 1,2,4 are unmarked; pre-mark ref 4 so the toggle flips it back.
    sqlx::query("UPDATE ttrss_user_entries SET marked = true WHERE ref_id = 4")
        .execute(state.db.pool())
        .await
        .unwrap();
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1, 4])),
        ("mode", json!(2)),
        ("field", json!(0)),
    ]))
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
}

/// `updateArticle` field 3 (`note`): store the note text.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_note(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1])),
        ("field", json!(3)),
        ("data", json!("hello & goodbye")),  // `clean()` strips tags; no tags here
    ]))
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
}

/// `updateArticle` field 4 (`score`): store the score (rejects `0`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_score(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1, 2])),
        ("field", json!(4)),
        ("data", json!("5")),
    ]))
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
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1])),
        ("field", json!(4)),
        ("data", json!("0")),
    ]))
    .await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "INCORRECT_USAGE");
}

/// `updateArticle` with no `article_ids` → `INCORRECT_USAGE`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_incorrect_usage_empty(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[("field", json!(0)), ("mode", json!(1))])).await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "INCORRECT_USAGE");
}

/// `updateArticle` field 0 (`marked`) with a non-0/1/2 mode → `INCORRECT_USAGE`.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_update_incorrect_usage_badmode(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::update_article(&state, &sess, &request(&[
        ("article_ids", json!([1])),
        ("field", json!(0)),
        ("mode", json!(3)),
    ]))
    .await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "INCORRECT_USAGE");
}

/// `unsubscribeFeed` removes an owned feed (or `E_OPERATION_FAILED` if absent).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_unsubscribe_ok(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::unsubscribe_feed(&state, &sess, &request(&[("feed_id", json!(1))])).await;
    assert_eq!(status, 0);
    assert_eq!(content["status"], "OK");
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE id = 1",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0, "feed 1 not removed");
    // Missing feed → `E_OPERATION_FAILED`.
    let (status, content) = api::unsubscribe_feed(&state, &sess, &request(&[("feed_id", json!(999))])).await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "E_OPERATION_FAILED");
}

/// `setArticleLabel` assign=true attaches label 1 (`label_id = 1026`).
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_label_assign(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::set_article_label(&state, &sess, &request(&[
        ("article_ids", json!([1, 2])),
        ("label_id", json!(1026)),
        ("assign", json!(true)),
    ]))
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
}

/// `setArticleLabel` assign=false removes label 1 from the entry.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_label_remove(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    // Pre-attach label 1 to ref 1.
    sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 1)")
        .execute(state.db.pool())
        .await
        .unwrap();
    let (status, content) = api::set_article_label(&state, &sess, &request(&[
        ("article_ids", json!([1])),
        ("label_id", json!(1026)),
        ("assign", json!(false)),
    ]))
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
}

/// `setArticleLabel` with an unknown label id → no rows updated.
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_label_missing(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::set_article_label(&state, &sess, &request(&[
        ("article_ids", json!([1])),
        ("label_id", json!(99999)),
        ("assign", json!(true)),
    ]))
    .await;
    assert_eq!(status, 0);
    assert_eq!(content["updated"], 0);
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_share_to_published_ok(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();
    let (status, content) = api::share_to_published(&state, &sess, &request(&[
        ("title", json!("My shared post")),
        ("url", json!("https://example.com/article-1")),
        ("content", json!("<p>Hello <b>world</b></p>")),
    ]))
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
    assert!(ts.expect("tsvector set").contains("world"), "tsvector holds the text");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_share_to_published_invalid_url(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::share_to_published(&state, &sess, &request(&[
        ("title", json!("Nope")),
        ("url", json!("not a url")),
        ("content", json!("body")),
    ]))
    .await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "E_OPERATION_FAILED");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_share_to_published_no_title_or_url(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let (status, content) = api::share_to_published(&state, &sess, &request(&[
        ("title", json!("")),
        ("url", json!("")),
        ("content", json!("body")),
    ]))
    .await;
    assert_eq!(status, 1);
    assert_eq!(content["error"], "E_OPERATION_FAILED");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_share_to_published_updates_existing(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();

    // Share the same URL twice (different content).
    for content in ["<p>first</p>", "<p>second body</p>"] {
        let (status, _) = api::share_to_published(&state, &sess, &request(&[
            ("title", json!("Post")),
            ("url", json!("https://example.com/same")),
            ("content", json!(content)),
        ]))
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
    assert_eq!(stored, "second body", "content is clean()ed (strip_tags + trim)");
}

