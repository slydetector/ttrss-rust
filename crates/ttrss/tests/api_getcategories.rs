//! DB tests for `API::getCategories` (M5a.2: the `enable_nested` param).
//!
//! The tt-rss-android client sends `enable_nested=true` to get **top-level
//! categories only**, with every descendant's unread folded into each root
//! (`RootCategoriesModel.java:95`). PHP then runs the category query with
//! `where_null('parent_cat')` and adds `Feeds::_get_cat_children_unread` to
//! each root's (direct-feeds-only) counter.
//!
//! Fixture baseline (owner 1): cat 1 "Technology" (feeds 1-3, 6 direct
//! unread), cat 2 "Science & Nature" (feed 4, 2 unread). The tests add a
//! nested cat 3 "Hardware" under cat 1 with one unread entry.

mod common;

use serde_json::{json, Map, Value};
use ttrss::api;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

/// Fixtures + a session carrying `uid = 1` (owner of all fixture rows) + the
/// per-user API gate enabled (the `before_and_dispatch` auth check).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
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

/// The nested fixture the cases build on: cat 3 "Hardware" under cat 1, feed
/// 5 in cat 3, one unread entry on feed 5.
async fn add_nested_cat(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (id, owner_uid, parent_cat, title)
         VALUES (3, 1, 1, 'Hardware')",
    )
    .execute(pool)
    .await
    .expect("subcat insert");
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Hardware feed', 3, 'https://example.com/hw', 60, 0, 'https://example.com')",
    )
    .execute(pool)
    .await
    .expect("feed insert");
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
         VALUES (9, 'uuid-hw-001', 5, 1, true, false, '', '')",
    )
    .execute(pool)
    .await
    .expect("entry insert");
}

/// The real-category entries (id >= 1) as `id → unread`, dropping the special
/// cats (covered by the corpus).
fn real_cats(content: &Value) -> std::collections::BTreeMap<i64, i64> {
    let mut m = std::collections::BTreeMap::new();
    for e in content.as_array().expect("array response") {
        let id = e.get("id").and_then(Value::as_i64).expect("id");
        if id >= 1 {
            m.insert(
                id,
                e.get("unread").and_then(Value::as_i64).expect("unread"),
            );
        }
    }
    m
}

#[tokio::test]
async fn test_flat_lists_nested_cat_and_omits_descendant_unread() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;
        add_nested_cat(state.db.pool()).await;

        // No `enable_nested`: every category (roots and nested) is its own entry,
        // and each counter covers only its direct feeds.
        let (status, content) =
            api::before_and_dispatch(&state, &sess, "getcategories", &request(&[]), None).await;
        assert_eq!(status, 0);
        let cats = real_cats(&content);
        assert_eq!(cats.get(&1), Some(&6), "cat 1 direct unread; got {cats:?}");
        assert_eq!(cats.get(&2), Some(&2), "cat 2 direct unread; got {cats:?}");
        assert_eq!(cats.get(&3), Some(&1), "nested cat 3 listed; got {cats:?}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_enable_nested_lists_roots_only_and_folds_descendants() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;
        add_nested_cat(state.db.pool()).await;

        // `enable_nested` (the client's param): roots only, descendants' unread
        // folded in (cat 1 = 6 direct + 1 in nested cat 3).
        let (status, content) = api::before_and_dispatch(
            &state,
            &sess,
            "getcategories",
            &request(&[("enable_nested", json!("true"))]),
            None,
        )
        .await;
        assert_eq!(status, 0);
        let cats = real_cats(&content);
        assert_eq!(cats.get(&1), Some(&7), "cat 1 incl. descendants; got {cats:?}");
        assert_eq!(cats.get(&2), Some(&2), "cat 2 unchanged; got {cats:?}");
        assert!(!cats.contains_key(&3), "nested cat 3 must be absent; got {cats:?}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_enable_nested_unread_only_drops_zero_unread_roots() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;
        add_nested_cat(state.db.pool()).await;
        // Read all of cat 2's feed so the root has zero unread.
        sqlx::query("UPDATE ttrss_user_entries SET unread = false WHERE feed_id = 4")
            .execute(state.db.pool())
            .await
            .expect("mark read");

        let (status, content) = api::before_and_dispatch(
            &state,
            &sess,
            "getcategories",
            &request(&[
                ("enable_nested", json!("true")),
                ("unread_only", json!("true")),
            ]),
            None,
        )
        .await;
        assert_eq!(status, 0);
        let cats = real_cats(&content);
        assert_eq!(cats.get(&1), Some(&7), "cat 1 kept; got {cats:?}");
        assert!(!cats.contains_key(&2), "cat 2 (zero unread) dropped; got {cats:?}");
        Ok(())
    })).await.unwrap();
}

/// Without `include_empty`, the special cats are gated by `_is_cat_empty`,
/// which is `true` only for LABELS (no labels) / UNCATEGORIZED (no feeds).
/// SPECIAL is *never* empty (the PHP returns `false` for it), and its unread
/// is always 0. The fixture has labels, so LABELS is present; there are no
/// uncategorized feeds, so UNCATEGORIZED (id 0) is absent.
#[tokio::test]
async fn test_special_cats_without_include_empty() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;

        let (status, content) =
            api::before_and_dispatch(&state, &sess, "getcategories", &request(&[]), None).await;
        assert_eq!(status, 0);

        let ids: Vec<i64> = content
            .as_array()
            .expect("array response")
            .iter()
            .map(|e| e.get("id").and_then(Value::as_i64).expect("id"))
            .collect();
        assert!(ids.contains(&-2), "LABELS present (labels exist): {ids:?}");
        assert!(ids.contains(&-1), "SPECIAL never empty: {ids:?}");
        assert!(!ids.contains(&0), "UNCATEGORIZED absent (no feeds): {ids:?}");

        // SPECIAL's unread is always 0 (PHP `_get_cat_unread` short-circuits it).
        let special = content
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e.get("id").and_then(Value::as_i64) == Some(-1))
            .expect("SPECIAL entry");
        assert_eq!(special.get("unread").and_then(Value::as_i64), Some(0));
        Ok(())
    })).await.unwrap();
}
