//! DB tests for the scope arms of `Feeds::_catchup` (M3: mark feeds /
//! categories / virtual feeds / labels / tags read).
//!
//! Rust-only: the frozen PHP suite has no `_catchup` tests, so there is no
//! 1:1 parity mapping — these exercise the ported arms directly.
//!
//! The tests call the public handlers `ttrss::feeds::catchup_feed`
//! (and later `catchup_all`) on a fresh per-test database with the versioned
//! `schema` + `baseline` + `seed` fixtures: feeds 1-4 (categories 1-2),
//! labels 1-2, entries 1-11 for owner 1
//! (feed 1: refs 1,2 unread + 3 read; feed 2: refs 4-6 unread; feed 3: ref 7
//! unread + 8 read; feed 4: refs 9,10 unread + 11 read).

use crate::common;

use serde_json::{Map, Value, json};
use sqlx::AssertSqlSafe;
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::session::Sess;

/// The template database + a session carrying `uid = 1` (owner of all
/// fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = ttrss::core::session::Session::new();
    data.set("uid", ttrss::core::phpser::Value::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// One owner-1 entry row: the columns `_catchup` reads or writes.
struct Row {
    ref_id: i32,
    feed_id: i32,
    unread: bool,
    marked: bool,
    published: bool,
    last_read_set: bool,
}

/// The read state of every owner-1 entry, ordered by ref id.
async fn entry_rows(db: &Db) -> Vec<Row> {
    let rows: Vec<(i32, i32, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT ref_id, feed_id, unread, marked, published, \
         (last_read IS NOT NULL) \
         FROM ttrss_user_entries WHERE owner_uid = 1 ORDER BY ref_id",
    )
    .fetch_all(db.pool())
    .await
    .expect("entry rows");
    rows.into_iter()
        .map(
            |(ref_id, feed_id, unread, marked, published, last_read_set)| Row {
                ref_id,
                feed_id,
                unread,
                marked,
                published,
                last_read_set,
            },
        )
        .collect()
}

/// The universal `_catchup` semantics: in-scope rows that were unread become
/// read with `last_read = NOW()`; in-scope rows already read are untouched
/// (the UPDATE targets `unread = true` only); out-of-scope rows are
/// unchanged.
fn assert_catchup(before: &[Row], after: &[Row], in_scope: impl Fn(&Row) -> bool) {
    assert_eq!(before.len(), after.len(), "row count changed");
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(b.ref_id, a.ref_id, "row set/order changed");
        if in_scope(b) {
            assert!(!a.unread, "ref {} (in scope) still unread", a.ref_id);
            assert_eq!(
                a.last_read_set, b.unread,
                "ref {}: last_read set = {}, was unread = {}",
                a.ref_id, a.last_read_set, b.unread
            );
        } else {
            assert_eq!(
                (a.unread, a.last_read_set),
                (b.unread, b.last_read_set),
                "ref {} (out of scope) was modified",
                a.ref_id
            );
        }
    }
}

#[tokio::test]
async fn test_catchup_single_feed() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            let before = entry_rows(db).await;

            // `RPC::catchupFeed` with `feed_id = 1`: the single-feed arm of
            // `Feeds::_catchup` — only that feed's UNREAD entries are marked read.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("1"))]))
                    .await;
            assert!(
                result.get("counters").is_some(),
                "expected a counters reply, got {result}"
            );

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.feed_id == 1);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_category() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;
            let before = entry_rows(db).await;

            // `is_cat` + `feed_id = 1`: the category arm — category 1 contains
            // feeds 1, 2, 3 (feed 4 is in category 2).
            let result = ttrss::feeds::catchup_feed(
                &state,
                &sess,
                &request(&[("feed_id", json!("1")), ("is_cat", json!("1"))]),
            )
            .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| matches!(r.feed_id, 1..=3));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_all() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;
            let before = entry_rows(db).await;

            // `feed_id = -4` (FEED_ALL): every owner-1 entry is in scope.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("-4"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |_| true);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_starred() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Star the unread ref 5 so the scope (marked = true) contains an unread
            // entry; refs 3, 8, 11 are starred and already read in the seed.
            sqlx::query("UPDATE ttrss_user_entries SET marked = true WHERE ref_id = $1")
                .bind(5i32)
                .execute(db.pool())
                .await
                .expect("setup: star ref 5");
            let before = entry_rows(db).await;

            // `feed_id = -1` (FEED_STARRED): the Starred view — only starred entries
            // are marked read.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("-1"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.marked);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_published() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Publish the unread ref 6 so the scope (published = true) contains an
            // unread entry (the seed has no published entries).
            sqlx::query("UPDATE ttrss_user_entries SET published = true WHERE ref_id = $1")
                .bind(6i32)
                .execute(db.pool())
                .await
                .expect("setup: publish ref 6");
            let before = entry_rows(db).await;

            // `feed_id = -2` (FEED_PUBLISHED): the Published view.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("-2"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.published);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Label 1: unread refs 4 and 7 plus already-read ref 8 (the seed has
            // no label assignments).
            sqlx::query(
                "INSERT INTO ttrss_user_labels2 (label_id, article_id) \
         VALUES (1, 4), (1, 7), (1, 8)",
            )
            .execute(db.pool())
            .await
            .expect("setup: label 1 on refs 4, 7, 8");
            let labeled: Vec<i32> =
                sqlx::query_scalar("SELECT article_id FROM ttrss_user_labels2 WHERE label_id = 1")
                    .fetch_all(db.pool())
                    .await
                    .expect("labeled refs");
            let before = entry_rows(db).await;

            // `feed_id = -1026` = `LABEL_BASE_INDEX - 1 - 1`: label 1's virtual feed.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("-1026"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| labeled.contains(&r.ref_id));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_cat_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Entries belonging to >= 1 label, across two different labels.
            sqlx::query(
                "INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 4), (2, 7)",
            )
            .execute(db.pool())
            .await
            .expect("setup: label assignments");
            let labeled: Vec<i32> =
                sqlx::query_scalar("SELECT DISTINCT article_id FROM ttrss_user_labels2")
                    .fetch_all(db.pool())
                    .await
                    .expect("labeled refs");
            let before = entry_rows(db).await;

            // `is_cat` + `feed_id = -2` (CATEGORY_LABELS): the Labels category —
            // every entry in any label is in scope.
            let result = ttrss::feeds::catchup_feed(
                &state,
                &sess,
                &request(&[("feed_id", json!("-2")), ("is_cat", json!("1"))]),
            )
            .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| labeled.contains(&r.ref_id));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_tag() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Tag the unread ref 4 and the already-read ref 8 (the seed has no tag
            // rows); the arm joins `ttrss_tags.post_int_id` to `int_id`.
            let int_4: i32 =
                sqlx::query_scalar("SELECT int_id FROM ttrss_user_entries WHERE ref_id = 4")
                    .fetch_one(db.pool())
                    .await
                    .expect("int_id of ref 4");
            let int_8: i32 =
                sqlx::query_scalar("SELECT int_id FROM ttrss_user_entries WHERE ref_id = 8")
                    .fetch_one(db.pool())
                    .await
                    .expect("int_id of ref 8");
            for int_id in [int_4, int_8] {
                sqlx::query(
                    "INSERT INTO ttrss_tags (tag_name, owner_uid, post_int_id) \
             VALUES ('rust', 1, $1)",
                )
                .bind(int_id)
                .execute(db.pool())
                .await
                .expect("setup: tag rows");
            }
            let tagged: Vec<i32> = sqlx::query_scalar(
                "SELECT ue.ref_id FROM ttrss_tags t \
         JOIN ttrss_user_entries ue ON ue.int_id = t.post_int_id \
         WHERE t.tag_name = 'rust'",
            )
            .fetch_all(db.pool())
            .await
            .expect("tagged refs");
            let before = entry_rows(db).await;

            // Non-numeric `feed_id`: the tag arm.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("rust"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| tagged.contains(&r.ref_id));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_uncategorized() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // All four fixture feeds have a category, so add a feed without one plus
            // an unread entry for it.
            sqlx::query(
                "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url) \
         VALUES (5, 1, 'Uncat Test', 'https://example.com/uncat.xml')",
            )
            .execute(db.pool())
            .await
            .expect("setup: uncat feed");
            sqlx::query(
                "INSERT INTO ttrss_entries (id, title, guid, link, updated, content, \
         content_hash, date_entered, date_updated) VALUES \
         (12, 'Uncat entry', 'tag:example.com,2026:uncat-001', \
          'https://example.com/uncat-1', '2026-09-01 10:00:00', '<p>x</p>', \
          'SHA1:uncat000000000000000000000000000000000', \
          '2026-09-01 10:00:00', '2026-09-01 10:00:00')",
            )
            .execute(db.pool())
            .await
            .expect("setup: uncat entry");
            sqlx::query(
                "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, \
         marked, tag_cache, label_cache) VALUES (12, 'uuid-ue-012', 5, 1, true, false, '', '')",
            )
            .execute(db.pool())
            .await
            .expect("setup: uncat user entry");
            let before = entry_rows(db).await;

            // `is_cat` + `feed_id = 0` (CATEGORY_UNCATEGORIZED): `cat_id IS NULL`.
            let result = ttrss::feeds::catchup_feed(
                &state,
                &sess,
                &request(&[("feed_id", json!("0")), ("is_cat", json!("1"))]),
            )
            .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.feed_id == 5);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Insert an extra entry + unread user entry for owner 1 (feed 4).
/// `date_entered_sql` is a trusted test SQL expression (`NOW()`,
/// `NOW() - INTERVAL '3 days'`, …) so the window math stays on the
/// database's clock.
async fn add_entry(
    db: &Db,
    id: i32,
    uuid: &str,
    guid: &str,
    date_entered_sql: &str,
    score: Option<i32>,
) {
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO ttrss_entries (id, title, guid, link, updated, content, \
         content_hash, date_entered, date_updated) VALUES \
         ($1, 'Extra entry', $2, 'https://example.com/x', {date_entered_sql}, '<p>x</p>', \
          'SHA1:extra00000000000000000000000000000000000', \
          {date_entered_sql}, {date_entered_sql})"
    )))
    .bind(id)
    .bind(guid)
    .execute(db.pool())
    .await
    .expect("setup: extra entry");
    if let Some(score) = score {
        sqlx::query(
            "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, \
             marked, tag_cache, label_cache, score) \
             VALUES ($1, $2, 4, 1, true, false, '', '', $3)",
        )
        .bind(id)
        .bind(uuid)
        .bind(score)
        .execute(db.pool())
        .await
        .expect("setup: extra user entry (score)");
    } else {
        sqlx::query(
            "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, \
             marked, tag_cache, label_cache) \
             VALUES ($1, $2, 4, 1, true, false, '', '')",
        )
        .bind(id)
        .bind(uuid)
        .execute(db.pool())
        .await
        .expect("setup: extra user entry");
    }
}

#[tokio::test]
async fn test_catchup_fresh() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // All fixture entries are months old, so add two fresh (NOW()) unread
            // entries: one with the default score 0 (in scope) and one with score -1
            // (excluded by the `score >= 0` filter).
            add_entry(
                db,
                13,
                "uuid-ue-013",
                "tag:example.com,2026:fresh-001",
                "NOW()",
                None,
            )
            .await;
            add_entry(
                db,
                14,
                "uuid-ue-014",
                "tag:example.com,2026:fresh-002",
                "NOW()",
                Some(-1),
            )
            .await;
            let before = entry_rows(db).await;

            // `feed_id = -3` (FEED_FRESH): the Fresh view — entries entered within
            // the last `FRESH_ARTICLE_MAX_AGE` hours (default 24) with score >= 0.
            let result =
                ttrss::feeds::catchup_feed(&state, &sess, &request(&[("feed_id", json!("-3"))]))
                    .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.ref_id == 13);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_mode_1day() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // A NOW() entry (inside the 1-day window) and a 3-day-old entry (outside
            // it); every fixture entry is months old (outside it).
            add_entry(
                db,
                13,
                "uuid-ue-013",
                "tag:example.com,2026:fresh-001",
                "NOW()",
                None,
            )
            .await;
            add_entry(
                db,
                15,
                "uuid-ue-015",
                "tag:example.com,2026:old-001",
                "NOW() - INTERVAL '3 days'",
                None,
            )
            .await;
            let before = entry_rows(db).await;

            // `mode = 1day` adds `date_entered < NOW() - INTERVAL '1 day'`: only
            // entries OLDER than the window are caught (the "clear the backlog" case).
            let result = ttrss::feeds::catchup_feed(
                &state,
                &sess,
                &request(&[("feed_id", json!("-4")), ("mode", json!("1day"))]),
            )
            .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.ref_id != 13);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_catchup_mode_1week() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = &state.db;

            // Same shape as the 1day test: the 3-day-old entry is now INSIDE the
            // 1-week window, so only the months-old fixture entries are caught.
            add_entry(
                db,
                13,
                "uuid-ue-013",
                "tag:example.com,2026:fresh-001",
                "NOW()",
                None,
            )
            .await;
            add_entry(
                db,
                15,
                "uuid-ue-015",
                "tag:example.com,2026:old-001",
                "NOW() - INTERVAL '3 days'",
                None,
            )
            .await;
            let before = entry_rows(db).await;

            // `mode = 1week`: `date_entered < NOW() - INTERVAL '1 week'`.
            let result = ttrss::feeds::catchup_feed(
                &state,
                &sess,
                &request(&[("feed_id", json!("-4")), ("mode", json!("1week"))]),
            )
            .await;
            assert!(result.get("counters").is_some(), "got {result}");

            let after = entry_rows(db).await;
            assert_catchup(&before, &after, |r| r.ref_id <= 11);
            Ok(())
        })
    })
    .await
    .unwrap();
}
