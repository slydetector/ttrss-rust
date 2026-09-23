//! DB tests for `API::getFeeds` / `API::_api_get_feeds` (M5a.4).
//!
//! The port assembles a *mixed* flat list — labels, special/virtual feeds,
//! child categories (when `include_nested`), then real feeds — each block
//! gated by `unread_only`, in that block order. These cover the per-block
//! assembly, the exact real-feed entry shape, the `cat_id` filter semantics
//! (incl. the API-only `-3`/`-4` sentinels and `0` = uncategorized),
//! `unread_only`, and `limit`/`offset`.
//!
//! Fixture baseline (owner 1): cats 1 "Technology" (root), 2 "Science &
//! Nature" (root); feeds 1 HN (cat 1, 2 unread), 2 Register (cat 1, 3
//! unread), 3 Ars (cat 1, 1 unread), 4 MIT (cat 2, 2 unread); labels
//! test-label-1/-2 (no entries → 0 unread); 11 entries total; no
//! uncategorized feeds.

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

fn test_config() -> Config { common::test_config() }

/// Fixtures + a session carrying `uid = 1` (owner of all fixture rows) + the
/// per-user API gate enabled (the `before_and_dispatch` auth check).
async fn setup(
    url: &str,
) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(state.db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
    (state, sess)
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Dispatch `getfeeds` and return the content array.
async fn get_feeds(
    state: &AppState,
    sess: &Sess,
    pairs: &[(&str, Value)],
) -> Value {
    let (status, content) = api::before_and_dispatch(state, sess, "getfeeds", &request(pairs)).await;
    assert_eq!(status, 0, "getfeeds should succeed: {content}");
    content
}

fn ids(content: &Value) -> Vec<i64> {
    content
        .as_array()
        .expect("array response")
        .iter()
        .map(|e| e.get("id").and_then(Value::as_i64).expect("id"))
        .collect()
}

fn unread_of(content: &Value, id: i64) -> i64 {
    content
        .as_array()
        .expect("array")
        .iter()
        .find(|e| e.get("id").and_then(Value::as_i64) == Some(id))
        .and_then(|e| e.get("unread").and_then(Value::as_i64))
        .expect("entry with unread")
}


#[tokio::test]
async fn special_feeds_cat_minus_1() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let c = get_feeds(&state, &sess, &[("cat_id", json!(-1))]).await;
    // Six special feeds, PHP's fixed order: Starred, Published, Fresh, All,
    // Recently-read, Archived.
    assert_eq!(ids(&c), vec![-1, -2, -3, -4, -6, 0]);
    let titles: Vec<&str> = c
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Starred articles",
            "Published articles",
            "Fresh articles",
            "All articles",
            "Recently read",
            "Archived articles"
        ]
    );
    // Every entry is cat_id -1.
    for e in c.as_array().unwrap() {
        assert_eq!(e["cat_id"].as_i64(), Some(-1));
    }
    // "All articles" unread = total unread across all feeds (2+3+1+2 = 8).
    assert_eq!(unread_of(&c, -4), 8);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn labels_cat_minus_2() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let c = get_feeds(&state, &sess, &[("cat_id", json!(-2))]).await;
    // Both labels (0 unread each, but unread_only=false so they appear).
    let titles: std::collections::BTreeSet<String> = c
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        ["test-label-1".to_string(), "test-label-2".to_string()].into_iter().collect()
    );
    for e in c.as_array().unwrap() {
        assert_eq!(e["cat_id"].as_i64(), Some(-2));
        assert_eq!(e["unread"].as_i64(), Some(0));
    }
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn all_except_virtual_cat_minus_3_full_shape() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let c = get_feeds(&state, &sess, &[("cat_id", json!(-3))]).await;
    // All 4 real feeds, ordered by title (all order_id 0): Ars(3), HN(1),
    // MIT(4), Register(2).
    assert_eq!(ids(&c), vec![3, 1, 4, 2]);
    // Per-feed unread.
    assert_eq!(unread_of(&c, 1), 2);
    assert_eq!(unread_of(&c, 2), 3);
    assert_eq!(unread_of(&c, 3), 1);
    assert_eq!(unread_of(&c, 4), 2);
    // Exact PHP entry shape: every field present.
    let want: [&str; 10] = [
        "feed_url",
        "title",
        "id",
        "unread",
        "has_icon",
        "cat_id",
        "last_updated",
        "order_id",
        "last_error",
        "update_interval",
    ];
    for e in c.as_array().unwrap() {
        for k in want {
            assert!(e.get(k).is_some(), "missing field {k}: {e}");
        }
        // has_icon is a bool (no icon files in the test dir → false).
        assert_eq!(e["has_icon"].as_bool(), Some(false));
    }
    // cat_id echoes the feed's category.
    assert_eq!(
        c.as_array().unwrap()[0]["cat_id"].as_i64(),
        Some(1) // Ars Technica is in cat 1
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn real_cat_lists_only_that_cats_feeds() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    // Cat 1 (Technology) has feeds 1,2,3 and no children.
    let c = get_feeds(&state, &sess, &[("cat_id", json!(1))]).await;
    let mut got = ids(&c);
    got.sort();
    assert_eq!(got, vec![1, 2, 3]);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn uncategorized_cat_0_is_empty() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    // No uncategorized (cat_id NULL/0) feeds in the fixture → empty.
    let c = get_feeds(&state, &sess, &[("cat_id", json!(0))]).await;
    assert_eq!(c.as_array().unwrap().len(), 0);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn unread_only_drops_zero_unread_feeds() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    // Add a feed in cat 1 with no entries (0 unread).
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Empty feed', 1, 'https://example.com/empty', 60, 0, 'https://example.com')",
    )
    .execute(state.db.pool())
    .await
    .unwrap();

    // Without unread_only: 5 feeds (the empty one included).
    let all = get_feeds(&state, &sess, &[("cat_id", json!(-3))]).await;
    assert_eq!(all.as_array().unwrap().len(), 5);
    // With unread_only: the 0-unread feed is dropped → 4.
    let only = get_feeds(&state, &sess, &[("cat_id", json!(-3)), ("unread_only", json!(true))]).await;
    assert_eq!(only.as_array().unwrap().len(), 4);
    assert!(!ids(&only).contains(&5));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn limit_and_offset_paginate() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    // Ordered by title: Ars(3), HN(1), MIT(4), Register(2).
    let first = get_feeds(&state, &sess, &[("cat_id", json!(-3)), ("limit", json!(2))]).await;
    assert_eq!(ids(&first), vec![3, 1]);
    let second = get_feeds(
        &state,
        &sess,
        &[("cat_id", json!(-3)), ("limit", json!(2)), ("offset", json!(2))],
    )
    .await;
    assert_eq!(ids(&second), vec![4, 2]);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn include_nested_folds_child_cat_unread() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    // Add a child cat 3 under cat 1, with feed 5 (1 unread entry).
    let p = state.db.pool();
    sqlx::query("INSERT INTO ttrss_feed_categories (id, owner_uid, parent_cat, title) VALUES (3, 1, 1, 'Hardware')")
        .execute(p)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Hardware feed', 3, 'https://example.com/hw', 60, 0, 'https://example.com')",
    )
    .execute(p)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_entries (id, title, guid, link, updated, content, content_hash, date_entered, date_updated)\n         VALUES (50, 'HW item', 'tag:example.com,2026:hw-001', 'https://example.com/hw/1',\n                 '2026-05-25 10:00:00', '<p>hw</p>', 'SHA1:hw', '2026-05-25 10:00:00', '2026-05-25 10:00:00')",
    )
    .execute(p)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)\n         VALUES (50, 'uuid-hw-001', 5, 1, true, false, '', '')",
    )
    .execute(p)
    .await
    .unwrap();

    // cat_id=1 with include_nested: the child cat 3 appears (is_cat) with its
    // unread folded in (1), ahead of the real cat-1 feeds.
    let c = get_feeds(
        &state,
        &sess,
        &[("cat_id", json!(1)), ("include_nested", json!(true))],
    )
    .await;
    let child = c
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.get("is_cat") == Some(&json!(true)))
        .expect("child cat entry present");
    assert_eq!(child["id"].as_i64(), Some(3));
    assert_eq!(child["unread"].as_i64(), Some(1));
    // The real cat-1 feeds (1,2,3) are also present — entries without `is_cat`
    // (the child cat shares id 3 with the real feed 3, so filter by `is_cat`).
    let feed_ids: Vec<i64> = c
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e.get("is_cat").is_none())
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    let mut sorted = feed_ids.clone();
    sorted.sort();
    assert_eq!(sorted, vec![1, 2, 3]);
        Ok(())
    })).await.unwrap();
}
