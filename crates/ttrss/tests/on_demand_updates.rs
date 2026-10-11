//! M6c.5 — on-demand / web-triggered updates + `Feeds::add` first update.
//!
//! DB-backed tests that drive the on-demand update paths against a loopback
//! mock HTTP server (the network is isolated here): `Feeds::_subscribe` (the
//! subscribe engine → the immediate first update) and `API::updateFeed` (the
//! on-demand full update, which runs immediately — not gated by the daemon
//! loop's `Semaphore(N)` — with the in-flight dedup).

mod common;

use std::sync::Arc;

use common::{Canned, MockServer};
use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::api::update_feed;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::feeds::subscribe;
use ttrss::pluginhost::PluginHost;
use ttrss::session::Sess;

const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// Insert a user and return its id.
async fn create_user(db: &Db, uid: i32, access_level: i32) -> i32 {
    sqlx::query(
        "INSERT INTO ttrss_users (id, login, pwd_hash, access_level) VALUES ($1, $2, 'SHA1:00', $3)",
    )
    .bind(uid)
    .bind(format!("test-user-{uid}"))
    .bind(access_level)
    .execute(db.pool())
    .await
    .expect("create user");
    uid
}

/// Insert a feed owned by `owner_uid` and return its id.
async fn create_feed(db: &Db, id: i32, owner_uid: i32, feed_url: &str) -> i32 {
    sqlx::query("INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(owner_uid)
        .bind(format!("Test Feed {id}"))
        .bind(feed_url)
        .execute(db.pool())
        .await
        .expect("create feed");
    id
}

/// The feed's `title`.
async fn feed_title(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("feed title")
}

/// `ttrss_entries` count for a feed (entries ⋈ user_entries by ref_id).
async fn entry_count(db: &Db, feed_id: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON e.id = ue.ref_id WHERE ue.feed_id = $1",
    )
    .bind(feed_id)
    .fetch_one(db.pool())
    .await
    .expect("entry count")
}

fn make_state(db: Db, tc: &common::TestCfg) -> AppState {
    AppState::new(tc.cfg.clone(), db)
}

fn make_sess(uid: i64) -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(uid));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess
}

/// A mock server that serves the canned Atom feed (not HTML).
fn atom_mock() -> MockServer {
    MockServer::new(vec![Canned {
        status: 200,
        reason: "OK",
        headers: vec![("content-type", "application/atom+xml".to_string())],
        body: MOCK_FEED.as_bytes().to_vec(),
    }])
}

// `Feeds::_subscribe` → the immediate first update: the feed is created (code
// 1) and `update_basic_info` fills in the feed's title from the fetched feed.
#[tokio::test]
async fn test_subscribe_first_update() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let tc = common::test_cfg(); // per-test temp cache dir (dropped at test end)
            let cfg = tc.cfg.clone();
            let mock = atom_mock();
            let uid = create_user(&db, 300, 0).await;

            let host = Arc::new(PluginHost::startup(&[]));
            let rc = subscribe(
                &cfg,
                db.pool(),
                &host,
                &cfg.self_url,
                uid as i64,
                0,
                &mock.url("/feed"),
                0,
                "",
                "",
                0,
                false, // reference path: the daemon does the first fetch
            )
            .await;
            assert_eq!(rc.get("code").and_then(|v| v.as_i64()), Some(1), "{rc}");
            let feed_id = rc.get("feed_id").and_then(|v| v.as_i64()).unwrap();

            // The immediate first update filled in the feed's title (from the feed).
            assert_eq!(feed_title(&db, feed_id as i32).await, "Atom:base testfeed");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The web-dialog deviation (`immediate_update = true`): the feed's articles
// are in the database before `subscribe` returns, so the dialog's
// `Feeds.reload()` shows the feed with its unread counter already populated.
// The content fetched for validation is the content that's processed — no
// second HTTP request for the feed.
#[tokio::test]
async fn test_subscribe_immediate_update() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let tc = common::test_cfg(); // per-test temp cache dir (dropped at test end)
            let cfg = tc.cfg.clone();
            let mock = atom_mock();
            let uid = create_user(&db, 302, 0).await;

            let host = Arc::new(PluginHost::startup(&[]));
            let rc = subscribe(
                &cfg,
                db.pool(),
                &host,
                &cfg.self_url,
                uid as i64,
                0,
                &mock.url("/feed"),
                0,
                "",
                "",
                0,
                true, // the web-dialog path
            )
            .await;
            assert_eq!(rc.get("code").and_then(|v| v.as_i64()), Some(1), "{rc}");
            let feed_id = rc.get("feed_id").and_then(|v| v.as_i64()).unwrap();

            // The articles were processed before the response — no daemon pass
            // happened in between.
            assert!(
                entry_count(&db, feed_id as i32).await > 0,
                "immediate update left no articles"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `Feeds::_subscribe` for a feed the user already subscribes to: code 0 (no
// duplicate insert), the existing feed's id.
#[tokio::test]
async fn test_subscribe_existing_feed() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let tc = common::test_cfg(); // per-test temp cache dir (dropped at test end)
            let cfg = tc.cfg.clone();
            let mock = atom_mock();
            let uid = create_user(&db, 301, 0).await;
            let feed_id = create_feed(&db, 301, uid, &mock.url("/feed")).await;

            let host = Arc::new(PluginHost::startup(&[]));
            let rc = subscribe(
                &cfg,
                db.pool(),
                &host,
                &cfg.self_url,
                uid as i64,
                0,
                &mock.url("/feed"),
                0,
                "",
                "",
                0,
                false, // reference path: the daemon does the first fetch
            )
            .await;
            assert_eq!(rc.get("code").and_then(|v| v.as_i64()), Some(0), "{rc}");
            assert_eq!(
                rc.get("feed_id").and_then(|v| v.as_i64()),
                Some(feed_id as i64)
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `API::updateFeed` — the on-demand full update: the feed's articles are
// ingested immediately (the request blocks on the update, not queued behind
// the daemon loop's `Semaphore(N)`).
#[tokio::test]
async fn test_update_feed_on_demand() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let mock = atom_mock();
            let tc2 = common::test_cfg(); // keep the cache dir alive for the test
            let state = make_state(db.clone(), &tc2);
            let uid = create_user(&db, 302, 0).await;
            let feed_id = create_feed(&db, 302, uid, &mock.url("/feed")).await;
            let sess = make_sess(uid as i64);

            let body: Map<String, Value> =
                json!({ "feed_id": feed_id }).as_object().unwrap().clone();
            let (status, _content) = update_feed(&state, &sess, &body).await;
            assert_eq!(status, 0);

            // The on-demand update ingested the feed's article immediately.
            assert_eq!(entry_count(&db, feed_id).await, 1);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `API::updateFeed` dedup: a feed already in flight (claimed by the running
// daemon loop within the last 10 minutes) is a no-op — no re-fetch, no articles.
#[tokio::test]
async fn test_update_feed_dedup_when_in_flight() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let mock = atom_mock();
            let tc2 = common::test_cfg(); // keep the cache dir alive for the test
            let state = make_state(db.clone(), &tc2);
            let uid = create_user(&db, 303, 0).await;
            let feed_id = create_feed(&db, 303, uid, &mock.url("/feed")).await;
            // Mark the feed in-flight (the daemon loop just claimed it).
            sqlx::query("UPDATE ttrss_feeds SET last_update_started = NOW() WHERE id = $1")
                .bind(feed_id)
                .execute(db.pool())
                .await
                .expect("mark in-flight");
            let sess = make_sess(uid as i64);

            let body: Map<String, Value> =
                json!({ "feed_id": feed_id }).as_object().unwrap().clone();
            let (status, _content) = update_feed(&state, &sess, &body).await;
            assert_eq!(status, 0);

            // The in-flight feed was skipped (no re-fetch, no articles ingested).
            assert_eq!(entry_count(&db, feed_id).await, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}
