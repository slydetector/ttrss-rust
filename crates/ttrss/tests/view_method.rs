//! Rust-only regression tests for the `m` (method) param of the web
//! `get_headlines` op (`Feeds::view` → `_format_headlines_list`).
//!
//! The frozen PHP handles two method values (Feeds.php):
//! - `m=ForceUpdate` — resets the feed's `last_updated` / `last_update_started`
//!   to `'1970-01-01'` so the in-process updater re-fetches it. This is the
//!   UI's active-feed refresh (the frozen JS sends it when re-selecting the
//!   active feed at offset 0, Feeds.js:423).
//! - `m=MarkAllReadGR:<feed>` — `Feeds::_catchup($method_split[1], false)`.
//!   Legacy/other clients only; the frozen JS never sends it.
//!
//! The port read `m` and ignored it. The frozen PHP suite does not cover the
//! `view` method side effects, so these pin the corrected behavior.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::headlines::view;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

const SESSION_UID: i64 = 1;

async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(SESSION_UID));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

fn view_extra(feed: &str, method: &str) -> Map<String, Value> {
    Map::from_iter(vec![
        ("feed".to_string(), json!(feed)),
        ("m".to_string(), json!(method)),
    ])
}

/// True when the feed's both update stamps equal `'1970-01-01'` (PHP's reset).
async fn stamps_reset(p: &sqlx::Pool<sqlx::Postgres>, feed_id: i64) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(last_updated = '1970-01-01'::timestamp
              AND last_update_started = '1970-01-01'::timestamp, false)
         FROM ttrss_feeds WHERE id = $1",
    )
    .bind(feed_id)
    .fetch_one(p)
    .await
    .unwrap()
}

#[tokio::test]
async fn test_force_update_resets_feed_stamps() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let p = state.db.pool();

    // Feed 3 starts with NULL stamps; give it a recent update stamp.
    sqlx::query(
        "UPDATE ttrss_feeds SET last_updated = '2026-05-20 10:00:00', last_update_started = '2026-05-20 10:00:00' WHERE id = 3",
    )
    .execute(p)
    .await
    .unwrap();
    assert!(!stamps_reset(p, 3).await);

    let reply = view(&state, &sess, &view_extra("3", "ForceUpdate")).await;
    assert!(
        reply.get("headlines").is_some(),
        "a normal headlines reply (not an error feed): {reply:?}"
    );

    assert!(
        stamps_reset(p, 3).await,
        "ForceUpdate resets feed 3's update stamps to 1970-01-01"
    );
    // The other feeds are untouched (the PHP updates only the viewed feed).
    assert!(
        !stamps_reset(p, 1).await && !stamps_reset(p, 2).await && !stamps_reset(p, 4).await
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_force_update_ignores_tag_feed() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let p = state.db.pool();

    // `rust` is a tag name (article 1's tag_cache), so `$feed` is non-numeric
    // and the PHP's `is_numeric($feed)` guard skips the stamp update.
    let reply = view(&state, &sess, &view_extra("rust", "ForceUpdate")).await;
    assert!(reply.get("headlines").is_some(), "{reply:?}");

    for id in 1..=4 {
        assert!(
            !stamps_reset(p, id).await,
            "tag ForceUpdate leaves feed {id}'s stamps NULL"
        );
    }
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_mark_all_read_gr_catchups_scoped_feed() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let p = state.db.pool();

    // Feed 2 has 3 unread entries in the seed; the viewed feed (1) is
    // irrelevant — the scope comes from the method param.
    let reply = view(&state, &sess, &view_extra("1", "MarkAllReadGR:2")).await;
    assert!(reply.get("headlines").is_some(), "{reply:?}");

    let rows: Vec<(i32, bool, bool)> =
        sqlx::query_as("SELECT ref_id, unread, last_read IS NOT NULL FROM ttrss_user_entries WHERE feed_id = 2 AND owner_uid = $1 ORDER BY ref_id")
            .bind(SESSION_UID)
            .fetch_all(p)
            .await
            .unwrap();
    assert_eq!(rows.len(), 3, "feed 2's three seed entries");
    for (ref_id, unread, has_last_read) in &rows {
        assert!(!unread, "entry {ref_id} marked read");
        assert!(*has_last_read, "entry {ref_id} last_read stamped");
    }

    // The viewed feed's unread entries are untouched (scope is feed 2 only).
    let unread_in_feed1: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries WHERE feed_id = 1 AND owner_uid = $1 AND unread = true")
            .bind(SESSION_UID)
            .fetch_one(p)
            .await
            .unwrap();
    assert_eq!(unread_in_feed1, 2, "feed 1's unread entries unchanged");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_method_undefined_is_noop() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let p = state.db.pool();

    sqlx::query(
        "UPDATE ttrss_feeds SET last_updated = '2026-05-20 10:00:00', last_update_started = '2026-05-20 10:00:00' WHERE id = 3",
    )
    .execute(p)
    .await
    .unwrap();

    // The JS sends the string "undefined" for an absent `m`; PHP normalizes
    // it to "" — no side effects.
    let reply = view(&state, &sess, &view_extra("3", "undefined")).await;
    assert!(reply.get("headlines").is_some(), "{reply:?}");

    let still: bool = sqlx::query_scalar(
        "SELECT last_updated = '2026-05-20 10:00:00'::timestamp FROM ttrss_feeds WHERE id = 3",
    )
    .fetch_one(p)
    .await
    .unwrap();
    assert!(still, "m=undefined leaves the stamps untouched");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_mark_all_read_gr_without_scope_is_noop() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let p = state.db.pool();

    // No colon: `$method_split[1]` is null in PHP → `_catchup(null, false)`
    // → the tag branch with an empty name → nothing matched. Must not panic.
    let reply = view(&state, &sess, &view_extra("1", "MarkAllReadGR")).await;
    assert!(reply.get("headlines").is_some(), "{reply:?}");

    let unread_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries WHERE owner_uid = $1 AND unread = true")
            .bind(SESSION_UID)
            .fetch_one(p)
            .await
            .unwrap();
    assert_eq!(unread_total, 8, "all 8 seeded unread entries remain unread");
        Ok(())
    })).await.unwrap();
}
