//! M6c.7 — the lightweight batch smoke test (the §1 exit criterion).
//!
//! Replaces the dropped "golden-file parity of an update run" full-DB diff
//! (`docs/05-unified-architecture.md` §1): feed the pipeline a fixture *batch* of
//! feeds and assert a handful of **aggregate / cross-feed invariants** — the
//! total new entries, a specific known article present, the counter (unread)
//! totals, and one cross-feed dedup hit. This catches *wiring* and *emergent /
//! cross-feed* bugs (the only thing a whole-DB diff uniquely provided) at a
//! fraction of the cost.
//!
//! The batch is replayed in-process through the M6c.2 workhorse
//! (`fetch_one_feed`: fetch → conditional-refetch decision → the M6b
//! `process_feed` pipeline) against a loopback [`MockServer`] (the network is
//! isolated here), reusing the same fixtures + helpers as the other DB-backed
//! update tests.

mod common;

use std::sync::Arc;
use ttrss::core::db::Db;
use ttrss::pluginhost::PluginHost;
use ttrss::updates::{FeedUpdateFlags, fetch_one_feed};
/// An empty plugin host — the `HOOK_ARTICLE_FILTER` chain is a no-op with
/// no plugins (M7.9), so these tests exercise the plugin-free path.
fn no_plugins() -> PluginHost {
    PluginHost::startup(&[])
}

use common::{Canned, MockServer};
use test_db::template::with_test_db;

/// Feed A: three entries — the shared article + two unique.
const FEED_A: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Feed A</title>
  <id>tag:example.com,a</id>
  <updated>2021-05-21T14:17:43+04:00</updated>
  <entry>
    <title>Shared article</title>
    <id>tag:example.com,shared-1</id>
    <link rel="alternate" type="text/html" href="http://example.com/shared-1" />
    <updated>2021-05-21T07:54:43+04:00</updated>
    <content type="html"><p>Shared content</p></content>
  </entry>
  <entry>
    <title>A-1</title>
    <id>tag:example.com,a-1</id>
    <link rel="alternate" type="text/html" href="http://example.com/a-1" />
    <updated>2021-05-21T07:55:43+04:00</updated>
  </entry>
  <entry>
    <title>A-2</title>
    <id>tag:example.com,a-2</id>
    <link rel="alternate" type="text/html" href="http://example.com/a-2" />
    <updated>2021-05-21T07:56:43+04:00</updated>
  </entry>
</feed>
"#;

/// Feed B: two entries — the shared article (same fields as in Feed A, so the
/// same `content_hash`) + one unique.
const FEED_B: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Feed B</title>
  <id>tag:example.com,b</id>
  <updated>2021-05-21T14:17:43+04:00</updated>
  <entry>
    <title>Shared article</title>
    <id>tag:example.com,shared-1</id>
    <link rel="alternate" type="text/html" href="http://example.com/shared-1" />
    <updated>2021-05-21T07:54:43+04:00</updated>
    <content type="html"><p>Shared content</p></content>
  </entry>
  <entry>
    <title>B-1</title>
    <id>tag:example.com,b-1</id>
    <link rel="alternate" type="text/html" href="http://example.com/b-1" />
    <updated>2021-05-21T07:57:43+04:00</updated>
  </entry>
</feed>
"#;

/// Feed C: one entry (unique).
const FEED_C: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Feed C</title>
  <id>tag:example.com,c</id>
  <updated>2021-05-21T14:17:43+04:00</updated>
  <entry>
    <title>C-1</title>
    <id>tag:example.com,c-1</id>
    <link rel="alternate" type="text/html" href="http://example.com/c-1" />
    <updated>2021-05-21T07:58:43+04:00</updated>
  </entry>
</feed>
"#;

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
    // `favicon_is_custom = true` suppresses the update pipeline's favicon
    // fetch (it is gated on `favicon_needs_check`, which is false when the
    // icon is custom). With the extended-URL guard now on (but bypassed in the
    // test env), the favicon fetch would otherwise issue extra `/` +
    // `/favicon.ico` requests that consume the mock's connection-ordered canned
    // responses and misalign the per-feed bodies. This test asserts cross-feed
    // dedup, not favicons.
    // `site_url` is set (a resolved-subscription shape): an empty `site_url`
    // would trigger the RSSUtils:404-409 basic-info fetch at update start,
    // issuing an extra request that consumes the mock's connection-ordered
    // canned responses and misaligns the per-feed bodies (the same hazard the
    // `favicon_is_custom` above suppresses).
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url, site_url, favicon_is_custom) \
         VALUES ($1, $2, $3, $4, $5, true)",
    )
    .bind(id)
    .bind(owner_uid)
    .bind(format!("Test Feed {id}"))
    .bind(feed_url)
    .bind(format!("http://example.test/site-{id}"))
    .execute(db.pool())
    .await
    .expect("create feed");
    id
}

/// The total `ttrss_entries` count (the deduped article set).
async fn entry_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries")
        .fetch_one(db.pool())
        .await
        .expect("entry count")
}

/// The total `ttrss_user_entries` count (per-feed subscriptions to articles).
async fn user_entry_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries")
        .fetch_one(db.pool())
        .await
        .expect("user entry count")
}

/// The total unread `ttrss_user_entries` count (the counter-cache total).
async fn unread_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_user_entries WHERE unread = true")
        .fetch_one(db.pool())
        .await
        .expect("unread count")
}

/// The number of `ttrss_entries` rows matching a title.
async fn entry_title_count(db: &Db, title: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries WHERE title = $1")
        .bind(title)
        .fetch_one(db.pool())
        .await
        .expect("title count")
}

// The batch smoke test: feed the pipeline a fixture batch of three feeds (6
// feed-subscriptions, 5 distinct articles — the shared article appears in both
// Feed A and Feed B with an identical `content_hash`), then assert the
// aggregate / cross-feed invariants.
#[tokio::test]
async fn test_batch_smoke() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let cfg = common::test_config();
            let mock = MockServer::new(vec![
                Canned {
                    status: 200,
                    reason: "OK",
                    headers: vec![("content-type", "application/atom+xml".to_string())],
                    body: FEED_A.as_bytes().to_vec(),
                },
                Canned {
                    status: 200,
                    reason: "OK",
                    headers: vec![("content-type", "application/atom+xml".to_string())],
                    body: FEED_B.as_bytes().to_vec(),
                },
                Canned {
                    status: 200,
                    reason: "OK",
                    headers: vec![("content-type", "application/atom+xml".to_string())],
                    body: FEED_C.as_bytes().to_vec(),
                },
            ]);

            let uid = create_user(&db, 400, 0).await;
            // The batch: three feeds, each pointing at the mock (the server serves the
            // canned responses in order, so the feeds are fetched A → B → C).
            let feed_a = create_feed(&db, 401, uid, &mock.url("/feed-a")).await;
            let feed_b = create_feed(&db, 402, uid, &mock.url("/feed-b")).await;
            let feed_c = create_feed(&db, 403, uid, &mock.url("/feed-c")).await;

            // The fixture baseline (the seed's pre-existing articles), so the
            // assertions measure the batch's contribution (a before/after delta) rather
            // than an absolute total.
            let entries_before = entry_count(&db).await;
            let user_entries_before = user_entry_count(&db).await;
            let unread_before = unread_count(&db).await;

            // Replay the batch through the M6c.2 workhorse (fetch → pipeline).
            let mut total_new = 0;
            for feed_id in [feed_a, feed_b, feed_c] {
                let outcome = fetch_one_feed(
                    db.pool(),
                    &cfg,
                    Arc::new(no_plugins()),
                    feed_id,
                    &FeedUpdateFlags::default(),
                    None,
                )
                .await
                .expect("fetch");
                assert!(
                    outcome.success,
                    "feed {feed_id} failed: {:?}",
                    outcome.error
                );
                total_new += outcome.new_articles;
            }

            // Invariant 1 — the total new entries: 6 feed items (3 + 2 + 1) collapse
            // to 5 distinct articles — the shared article appears in Feed A and Feed B
            // with an identical `content_hash`, so it is deduped to one `ttrss_entries`
            // row (the cross-feed dedup hit).
            assert_eq!(
                entry_count(&db).await - entries_before,
                5,
                "expected 5 new distinct articles (6 feed items - 1 deduped)"
            );

            // Invariant 2 — the per-user subscriptions: 5 new `ttrss_user_entries`
            // (`ttrss_user_entries` is deduped by `(ref_id, owner_uid)`, so the shared
            // article — the same `ref_id` in both feeds — is subscribed once, not twice).
            assert_eq!(
                user_entry_count(&db).await - user_entries_before,
                5,
                "expected 5 new per-user subscriptions"
            );

            // Invariant 3 — the workhorse reports 5 new articles (3 + 2 + 1 - 1 deduped).
            assert_eq!(
                total_new, 5,
                "expected the workhorse to report 5 new articles"
            );

            // Invariant 4 — the shared article is present exactly once (deduped across
            // Feed A and Feed B), even though it appeared in two feeds.
            assert_eq!(
                entry_title_count(&db, "Shared article").await,
                1,
                "expected the shared article deduped to one row"
            );

            // Invariant 5 — the counter-cache total: 5 new unread subscriptions (all
            // new articles are unread).
            assert_eq!(
                unread_count(&db).await - unread_before,
                5,
                "expected 5 new unread subscriptions"
            );

            // Invariant 6 — the cross-feed dedup is observable at the aggregate level:
            // 6 feed items were fetched (3 + 2 + 1) but only 5 distinct articles
            // remain, proving the cross-feed `content_hash` dedup fired (not merely a
            // per-feed entry count).
            assert_eq!(
                entry_count(&db).await - entries_before,
                5,
                "expected the cross-feed dedup to collapse 6 items to 5 articles"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
