//! Test parity: 1:1 port of
//! `tt-rss/tests_functional/LabelsTest.php` (15 pure-function cases) and
//! `tt-rss/tests_integration/LabelsIntegrationTest.php` (35 cases).
//!
//! The PHP `Labels` class is ported to `ttrss::labels`; the
//! integration cases run on the fresh per-test database with the
//! versioned `schema` + `baseline` + `seed` fixtures (seed labels:
//! id 1 'test-label-1', id 2 'test-label-2', owner 1).

use crate::common;

use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::core::db::Db;
use ttrss::labels;

// ── LabelsTest (pure functions) ─────────────────────────────────────────────

// PHP: test_label_to_feed_id_basic
#[test]
fn test_label_to_feed_id_basic() {
    assert_eq!(-1026, labels::label_to_feed_id(1));
}

// PHP: test_label_to_feed_id_larger_label
#[test]
fn test_label_to_feed_id_larger_label() {
    assert_eq!(-1030, labels::label_to_feed_id(5));
}

// PHP: test_label_to_feed_id_zero
#[test]
fn test_label_to_feed_id_zero() {
    assert_eq!(-1025, labels::label_to_feed_id(0));
}

// PHP: test_label_to_feed_id_negative_input
#[test]
fn test_label_to_feed_id_negative_input() {
    assert_eq!(-1028, labels::label_to_feed_id(-3));
    assert_eq!(labels::label_to_feed_id(3), labels::label_to_feed_id(-3));
}

// PHP: test_feed_to_label_id_basic
#[test]
fn test_feed_to_label_id_basic() {
    assert_eq!(labels::LABEL_BASE_INDEX, labels::feed_to_label_id(1));
}

// PHP: test_feed_to_label_id_larger_feed
#[test]
fn test_feed_to_label_id_larger_feed() {
    assert_eq!(-1020, labels::feed_to_label_id(5));
}

// PHP: test_feed_to_label_id_zero
#[test]
fn test_feed_to_label_id_zero() {
    assert_eq!(-1025, labels::feed_to_label_id(0));
}

// PHP: test_feed_to_label_id_negative_input
#[test]
fn test_feed_to_label_id_negative_input() {
    assert_eq!(-1023, labels::feed_to_label_id(-2));
    assert_eq!(labels::feed_to_label_id(2), labels::feed_to_label_id(-2));
}

// PHP: test_round_trip_label_to_feed
#[test]
fn test_round_trip_label_to_feed() {
    let label = 1;
    let feed_id = labels::label_to_feed_id(label);
    let round_trip = labels::feed_to_label_id(feed_id);
    assert_eq!(label, round_trip);
}

// PHP: test_round_trip_label_to_feed_multiple
#[test]
fn test_round_trip_label_to_feed_multiple() {
    for label in [1i32, 5, 10, 100, 500] {
        let feed_id = labels::label_to_feed_id(label);
        let round_trip = labels::feed_to_label_id(feed_id);
        assert_eq!(label, round_trip, "Round-trip failed for label={label}");
    }
}

// PHP: test_feed_to_label_is_not_invertible
#[test]
fn test_feed_to_label_is_not_invertible() {
    let feed = 1;
    let label_id = labels::feed_to_label_id(feed);
    let round_trip = labels::label_to_feed_id(label_id);
    // The reverse direction is NOT an inverse — expected behavior.
    assert_ne!(feed, round_trip);
}

// PHP: test_label_and_feed_ranges_non_overlapping
#[test]
fn test_label_and_feed_ranges_non_overlapping() {
    let label_threshold = labels::LABEL_BASE_INDEX - 2; // -1026
    let feed_threshold = labels::LABEL_BASE_INDEX - 1; // -1025

    // All positive label conversions produce values at or below the
    // label threshold.
    for label in [1i32, 5, 100, 500] {
        assert!(
            labels::label_to_feed_id(label) <= label_threshold,
            "label_to_feed_id({label}) should be <= {label_threshold}"
        );
    }

    // All positive feed conversions produce values at or above the
    // feed threshold.
    for feed in [1i32, 5, 100, 500] {
        assert!(
            labels::feed_to_label_id(feed) >= feed_threshold,
            "feed_to_label_id({feed}) should be >= {feed_threshold}"
        );
    }

    // Verify the gap: label range max < feed range min.
    assert!(labels::label_to_feed_id(1) < labels::feed_to_label_id(1));
}

// PHP: test_label_to_feed_id_monotonic
#[test]
fn test_label_to_feed_id_monotonic() {
    // assertLessThan(labelToFeedId(2), labelToFeedId(1)): the id-space is
    // negative, so larger labels map to smaller feed ids.
    assert!(labels::label_to_feed_id(1) > labels::label_to_feed_id(2));
    // assertLessThan(labelToFeedId(5), labelToFeedId(10))
    assert!(labels::label_to_feed_id(10) < labels::label_to_feed_id(5));
}

// PHP: test_feed_to_label_id_monotonic
#[test]
fn test_feed_to_label_id_monotonic() {
    assert!(labels::feed_to_label_id(1) < labels::feed_to_label_id(2));
    assert!(labels::feed_to_label_id(5) < labels::feed_to_label_id(10));
}

// PHP: test_zero_label_and_feed_produce_same_value
#[test]
fn test_zero_label_and_feed_produce_same_value() {
    assert_eq!(labels::label_to_feed_id(0), labels::feed_to_label_id(0));
}

// ── LabelsIntegrationTest ───────────────────────────────────────────────────

async fn setup(url: &str) -> Db {
    common::make_db_from_url(url).await.expect("test db")
}

/// PHP helper `countUserLabels`.
async fn count_user_labels(db: &Db, label_id: i32, article_id: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_labels2 WHERE label_id = $1 AND article_id = $2",
    )
    .bind(label_id as i64)
    .bind(article_id)
    .fetch_one(db.pool())
    .await
    .expect("count user labels")
}

/// PHP helper `getLabelCache` (owner 1).
async fn get_label_cache(db: &Db, ref_id: i32) -> String {
    let cache: Option<String> = sqlx::query_scalar(
        "SELECT label_cache FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
    )
    .bind(ref_id)
    .bind(1i64)
    .fetch_optional(db.pool())
    .await
    .expect("label cache row");
    cache.unwrap_or_default()
}

/// PHP: test_find_id_existing_label
#[tokio::test]
async fn test_find_id_existing_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed.sql inserts label id=1 with caption 'test-label-1'
            let id = labels::find_id(db.pool(), "test-label-1", 1).await;
            assert_eq!(1, id, "Should find the label by exact caption");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_id_non_existing_label
#[tokio::test]
async fn test_find_id_non_existing_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let id = labels::find_id(db.pool(), "nonexistent-label", 1).await;
            assert_eq!(0, id, "Should return 0 for a label that does not exist");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_id_case_insensitive
#[tokio::test]
async fn test_find_id_case_insensitive() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let id_upper = labels::find_id(db.pool(), "TEST-LABEL-1", 1).await;
            let id_mixed = labels::find_id(db.pool(), "Test-Label-1", 1).await;
            let id_lower = labels::find_id(db.pool(), "test-label-1", 1).await;

            assert_eq!(1, id_upper, "Uppercase should find the label");
            assert_eq!(1, id_mixed, "Mixed case should find the label");
            assert_eq!(1, id_lower, "Lowercase should find the label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_id_different_owner
#[tokio::test]
async fn test_find_id_different_owner() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed data is for uid=1; uid=999 has no labels
            let id = labels::find_id(db.pool(), "test-label-1", 999).await;
            assert_eq!(0, id, "Should not find label for a different owner");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_caption_existing
#[tokio::test]
async fn test_find_caption_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let caption = labels::find_caption(db.pool(), 1, 1).await;
            assert_eq!("test-label-1", caption);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_caption_non_existing
#[tokio::test]
async fn test_find_caption_non_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let caption = labels::find_caption(db.pool(), 9999, 1).await;
            assert_eq!("", caption);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_find_caption_different_owner
#[tokio::test]
async fn test_find_caption_different_owner() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let caption = labels::find_caption(db.pool(), 1, 999).await;
            assert_eq!("", caption);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_all_returns_seed_labels
#[tokio::test]
async fn test_get_all_returns_seed_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let all = labels::get_all(db.pool(), 1).await;
            assert_eq!(2, all.len());
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_all_ordered_by_caption
#[tokio::test]
async fn test_get_all_ordered_by_caption() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let all = labels::get_all(db.pool(), 1).await;
            // Seed: 'test-label-1' (id=1), 'test-label-2' (id=2)
            assert_eq!(
                "test-label-1",
                all[0].get("caption").and_then(Value::as_str).unwrap()
            );
            assert_eq!(
                "test-label-2",
                all[1].get("caption").and_then(Value::as_str).unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_all_empty_for_nonexistent_owner
#[tokio::test]
async fn test_get_all_empty_for_nonexistent_owner() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let all = labels::get_all(db.pool(), 9999).await;
            assert!(all.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_all_includes_color_fields
#[tokio::test]
async fn test_get_all_includes_color_fields() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let all = labels::get_all(db.pool(), 1).await;
            let first = &all[0];
            assert!(first.contains_key("id"));
            assert!(first.contains_key("caption"));
            assert!(first.contains_key("fg_color"));
            assert!(first.contains_key("bg_color"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_as_hash_returns_associative_array
#[tokio::test]
async fn test_get_as_hash_returns_associative_array() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let hash = labels::get_as_hash(db.pool(), 1).await;
            assert_eq!(2, hash.len());
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_as_hash_keys_are_ids
#[tokio::test]
async fn test_get_as_hash_keys_are_ids() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let hash = labels::get_as_hash(db.pool(), 1).await;
            assert!(hash.contains_key(&1));
            assert!(hash.contains_key(&2));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_get_as_hash_values_match_get_all
#[tokio::test]
async fn test_get_as_hash_values_match_get_all() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let hash = labels::get_as_hash(db.pool(), 1).await;
            let all = labels::get_all(db.pool(), 1).await;

            for label in &all {
                let id = label.get("id").and_then(Value::as_i64).unwrap() as i32;
                let from_hash: &Map<String, Value> = hash.get(&id).unwrap();
                assert_eq!(*from_hash, *label);
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_create_new_label
#[tokio::test]
async fn test_create_new_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let result = labels::create(
                &db,
                "integration-test-label",
                Some("#ff0000"),
                Some("#000000"),
                1,
                None,
            )
            .await
            .expect("create label");
            assert_eq!(Some(1), result, "Should return 1 row inserted");

            // Verify it exists in the database
            let id = labels::find_id(db.pool(), "integration-test-label", 1).await;
            assert!(id > 0, "Label should be findable after creation");

            // Clean up
            labels::remove(&db, id, 1, None)
                .await
                .expect("remove label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_create_label_with_default_colors
#[tokio::test]
async fn test_create_label_with_default_colors() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            let result = labels::create(&db, "minimal-label", None, None, 1, None)
                .await
                .expect("create label");
            assert_eq!(Some(1), result);

            let id = labels::find_id(db.pool(), "minimal-label", 1).await;
            let caption = labels::find_caption(db.pool(), id, 1).await;
            assert_eq!("minimal-label", caption);

            // Clean up
            labels::remove(&db, id, 1, None)
                .await
                .expect("remove label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_create_duplicate_label_returns_false
#[tokio::test]
async fn test_create_duplicate_label_returns_false() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed has 'test-label-1'
            let result = labels::create(&db, "test-label-1", None, None, 1, None)
                .await
                .expect("create label");
            assert_eq!(
                None, result,
                "Creating a duplicate label should return false"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_create_duplicate_label_case_insensitive
#[tokio::test]
async fn test_create_duplicate_label_case_insensitive() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed has 'test-label-1'
            let result = labels::create(&db, "TEST-LABEL-1", None, None, 1, None)
                .await
                .expect("create label");
            assert_eq!(None, result, "Duplicate check should be case-insensitive");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_create_different_owner
#[tokio::test]
async fn test_create_different_owner() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // The test database only has uid=1 (admin). Creating for other UIDs
            // would require additional seed data, so we verify that uid=1 works and
            // that the owner_uid parameter is correctly passed through.
            let result = labels::create(
                &db,
                "owner-1-label",
                Some("#00ff00"),
                Some("#ffffff"),
                1,
                Some(1),
            )
            .await
            .expect("create label");
            assert_eq!(Some(1), result, "Should create label for uid=1");

            let id = labels::find_id(db.pool(), "owner-1-label", 1).await;
            assert!(id > 0);

            // Verify uid=999 cannot see it
            let id_for_uid999 = labels::find_id(db.pool(), "owner-1-label", 999).await;
            assert_eq!(0, id_for_uid999);

            // Clean up
            labels::remove(&db, id, 1, None)
                .await
                .expect("remove label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_remove_existing_label
#[tokio::test]
async fn test_remove_existing_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Create a label to remove
            let result = labels::create(
                &db,
                "to-be-removed",
                Some("#ff0000"),
                Some("#ffffff"),
                1,
                None,
            )
            .await
            .expect("create label");
            assert_eq!(Some(1), result);
            let id = labels::find_id(db.pool(), "to-be-removed", 1).await;
            assert!(id > 0);

            // Remove it
            labels::remove(&db, id, 1, None)
                .await
                .expect("remove label");

            // Verify it's gone
            let caption = labels::find_caption(db.pool(), id, 1).await;
            assert_eq!("", caption, "Label should be removed from DB");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_remove_cleans_label_cache
#[tokio::test]
async fn test_remove_cleans_label_cache() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Create a label
    let result = labels::create(
        &db,
        "cache-test-label",
        Some("#ff0000"),
        Some("#ffffff"),
        1,
        None,
    )
    .await
    .expect("create label");
    assert_eq!(Some(1), result);
    let id = labels::find_id(db.pool(), "cache-test-label", 1).await;
    assert!(id > 0);

    // Set a label_cache on an article
    sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = '{\"test\":true}' WHERE ref_id = 1 AND owner_uid = 1",
    )
    .execute(db.pool())
    .await
    .expect("set cache");

    // Verify cache was set
    let cache = get_label_cache(&db, 1).await;
    assert!(!cache.is_empty());

    // Remove the label — should clear all caches for uid=1
    labels::remove(&db, id, 1, None)
        .await
        .expect("remove label");

    // Verify cache was cleared
    let cache_after = get_label_cache(&db, 1).await;
    assert_eq!("", cache_after, "Removing a label should clear label_cache");
        Ok(())
    })).await.unwrap();
}

/// PHP: test_remove_nonexistent_label
#[tokio::test]
async fn test_remove_nonexistent_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Should not throw — removing a non-existent label is a no-op
            labels::remove(&db, 99999, 1, None)
                .await
                .expect("remove label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_add_article_to_existing_label
#[tokio::test]
async fn test_add_article_to_existing_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // seed has label id=1, caption='test-label-1'; article ref_id=1
            labels::add_article(db.pool(), 1, "test-label-1", 1).await;

            let count = count_user_labels(&db, 1, 1).await;
            assert_eq!(1, count, "Should have one entry in ttrss_user_labels2");

            // Verify label_cache was cleared
            let cache = get_label_cache(&db, 1).await;
            assert_eq!("", cache, "add_article should clear label_cache");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_add_article_to_nonexistent_label
#[tokio::test]
async fn test_add_article_to_nonexistent_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Label 'nonexistent' does not exist, so find_id returns 0 and
            // add_article returns early without modifying anything
            let initial_count = count_user_labels(&db, 0, 1).await;

            labels::add_article(db.pool(), 1, "nonexistent-label", 1).await;

            let count = count_user_labels(&db, 0, 1).await;
            assert_eq!(
                initial_count, count,
                "Should not add anything for nonexistent label"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_add_article_is_idempotent
#[tokio::test]
async fn test_add_article_is_idempotent() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            labels::add_article(db.pool(), 1, "test-label-1", 1).await;
            let count1 = count_user_labels(&db, 1, 1).await;

            labels::add_article(db.pool(), 1, "test-label-1", 1).await;
            let count2 = count_user_labels(&db, 1, 1).await;

            assert_eq!(
                count1, count2,
                "Adding same label twice should not create duplicates"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_add_article_different_article
#[tokio::test]
async fn test_add_article_different_article() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            labels::add_article(db.pool(), 1, "test-label-1", 1).await;
            let count1 = count_user_labels(&db, 1, 1).await;

            labels::add_article(db.pool(), 2, "test-label-1", 1).await;
            let count2 = count_user_labels(&db, 1, 2).await;

            assert_eq!(1, count1, "Article 1 should have the label");
            assert_eq!(1, count2, "Article 2 should have the label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_remove_article_from_existing_label
#[tokio::test]
async fn test_remove_article_from_existing_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // First add the label relation
            labels::add_article(db.pool(), 1, "test-label-1", 1).await;
            let count_before = count_user_labels(&db, 1, 1).await;
            assert_eq!(1, count_before);

            // Now remove it
            labels::remove_article(db.pool(), 1, "test-label-1", 1).await;

            let count_after = count_user_labels(&db, 1, 1).await;
            assert_eq!(0, count_after, "Article should be removed from label");
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_remove_article_nonexistent_relation
#[tokio::test]
async fn test_remove_article_nonexistent_relation() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Removing a label relation that doesn't exist should be a no-op
            let initial_count = count_user_labels(&db, 1, 9999).await;
            labels::remove_article(db.pool(), 9999, "test-label-1", 1).await;
            let final_count = count_user_labels(&db, 1, 9999).await;
            assert_eq!(initial_count, final_count);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_remove_article_clears_cache
#[tokio::test]
async fn test_remove_article_clears_cache() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Add a label to article 1
    labels::add_article(db.pool(), 1, "test-label-1", 1).await;

    // Manually set a label_cache
    sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = '{\"test\":true}' WHERE ref_id = 1 AND owner_uid = 1",
    )
    .execute(db.pool())
    .await
    .expect("set cache");

    // Remove the label
    labels::remove_article(db.pool(), 1, "test-label-1", 1).await;

    // Cache should be cleared
    let cache = get_label_cache(&db, 1).await;
    assert_eq!("", cache, "remove_article should clear label_cache");
        Ok(())
    })).await.unwrap();
}

/// PHP: test_clear_cache_clears_label_cache
#[tokio::test]
async fn test_clear_cache_clears_label_cache() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Set a label_cache
    sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = '{\"test\":true}' WHERE ref_id = 1 AND owner_uid = 1",
    )
    .execute(db.pool())
    .await
    .expect("set cache");

    let cache_before = get_label_cache(&db, 1).await;
    assert!(!cache_before.is_empty());

    // Clear it
    labels::clear_cache(db.pool(), 1).await;

    let cache_after = get_label_cache(&db, 1).await;
    assert_eq!("", cache_after);
        Ok(())
    })).await.unwrap();
}

/// PHP: test_clear_cache_nonexistent_article
#[tokio::test]
async fn test_clear_cache_nonexistent_article() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Should not throw even if article doesn't exist
            labels::clear_cache(db.pool(), 99999).await;
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_update_cache_force_mode
#[tokio::test]
async fn test_update_cache_force_mode() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Force mode first clears the cache, then repopulates from the
    // article's actual labels (via Article::_get_labels). Since article
    // 1 has no labels in ttrss_user_labels2, the result is an empty array.
    sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = '{\"existing\":true}' WHERE ref_id = 1 AND owner_uid = 1",
    )
    .execute(db.pool())
    .await
    .expect("set cache");

    let cache_before = get_label_cache(&db, 1).await;
    assert!(!cache_before.is_empty());

    // Force clear — should clear and then repopulate from DB (empty)
    labels::update_cache(&db, 1, 1, 1, &[], true).await;

    let cache_after = get_label_cache(&db, 1).await;
    // The cache is repopulated from the article's actual labels.
    // Since article 1 has no labels, it becomes an empty array '[]'.
    assert_eq!(
        "[]", cache_after,
        "Force mode should clear stale cache and repopulate from DB"
    );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_update_cache_with_labels
#[tokio::test]
async fn test_update_cache_with_labels() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // Labels format: [[label_id, caption, fg_color, bg_color], ...]
            let labels_arg = vec![json!([1, "test-label-1", "#ff0000", "#ffffff"])];
            labels::update_cache(&db, 1, 1, 1, &labels_arg, false).await;

            let cache = get_label_cache(&db, 1).await;
            assert!(!cache.is_empty(), "Cache should be set");

            let decoded: Value = serde_json::from_str(&cache).expect("valid json cache");
            let arr = decoded.as_array().expect("array cache");
            assert_eq!(1, arr.len());
            assert_eq!(
                1,
                arr[0][0].as_i64().unwrap(),
                "First element should be label_id"
            );
            assert_eq!(
                "test-label-1",
                arr[0][1].as_str().unwrap(),
                "Second element should be caption"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// PHP: test_update_cache_empty_labels
#[tokio::test]
async fn test_update_cache_empty_labels() {
    with_test_db(|url| Box::pin(async move {
    let db = setup(&url).await;
    // Set a label_cache first
    sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = '{\"existing\":true}' WHERE ref_id = 1 AND owner_uid = 1",
    )
    .execute(db.pool())
    .await
    .expect("set cache");

    // Update with empty labels (no force) — should not change anything
    labels::update_cache(&db, 1, 1, 1, &[], false).await;

    let cache = get_label_cache(&db, 1).await;
    assert!(
        !cache.is_empty(),
        "Empty labels without force should not modify cache"
    );
        Ok(())
    })).await.unwrap();
}

/// PHP: test_full_label_lifecycle
#[tokio::test]
async fn test_full_label_lifecycle() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = setup(&url).await;
            // 1. Create a new label
            let result = labels::create(
                &db,
                "lifecycle-test",
                Some("#0000ff"),
                Some("#ffffff"),
                1,
                None,
            )
            .await
            .expect("create label");
            assert_eq!(Some(1), result);

            // 2. Find it
            let id = labels::find_id(db.pool(), "lifecycle-test", 1).await;
            assert!(id > 0);

            // 3. Get its caption
            let caption = labels::find_caption(db.pool(), id, 1).await;
            assert_eq!("lifecycle-test", caption);

            // 4. It appears in get_all
            let all = labels::get_all(db.pool(), 1).await;
            let found = all.iter().any(|l| {
                l.get("id").and_then(Value::as_i64) == Some(id as i64)
                    && l.get("caption").and_then(Value::as_str) == Some("lifecycle-test")
            });
            assert!(found, "New label should appear in get_all()");

            // 5. Add it to an article
            labels::add_article(db.pool(), 1, "lifecycle-test", 1).await;
            assert_eq!(1, count_user_labels(&db, id, 1).await);

            // 6. Remove it from the article
            labels::remove_article(db.pool(), 1, "lifecycle-test", 1).await;
            assert_eq!(0, count_user_labels(&db, id, 1).await);

            // 7. Remove the label entirely
            labels::remove(&db, id, 1, None)
                .await
                .expect("remove label");
            assert_eq!("", labels::find_caption(db.pool(), id, 1).await);
            Ok(())
        })
    })
    .await
    .unwrap();
}
