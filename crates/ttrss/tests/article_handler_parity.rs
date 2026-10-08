//! Test parity: 1:1 port of `tt-rss/tests_integration/ArticleHandlerTest.php`
//! (17 cases).
//!
//! The PHP suite invokes the `Article` handler methods directly
//! (`$this->invokeHandler('Article', 'setScore', [...])`) with
//! `DbTestCase::setUp`'s `$_SESSION["uid"] = 1` on the same versioned
//! schema + baseline + seed fixtures; the Rust port calls the same handler
//! functions (`ttrss::article::{set_score, set_article_tags,
//! complete_tags, label_ops, metadata_by_id, redirect}`) with an explicit
//! `Sess` carrying `uid = 1`.

mod common;

use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::core::db::Db;

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Read `score` for a ref id (owner 1).
async fn score_of(db: &Db, ref_id: i32) -> i32 {
    sqlx::query_scalar("SELECT score FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = 1")
        .bind(ref_id)
        .fetch_one(db.pool())
        .await
        .expect("score")
}

/// The `int_id` of ref `1` (owner 1) — the PHP helper `getIntIdForRef(1)`.
async fn int_id_for_ref1(db: &Db) -> i32 {
    sqlx::query_scalar("SELECT int_id FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1")
        .fetch_one(db.pool())
        .await
        .expect("int_id")
}

/// `ttrss_tags` row count for ref 1's int_id (the PHP assertions' query).
async fn tag_row_count(db: &Db) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_tags WHERE post_int_id = (SELECT int_id FROM ttrss_user_entries WHERE ref_id = 1)",
    )
    .fetch_one(db.pool())
    .await
    .expect("tag count")
}

// ── setScore() ──────────────────────────────────────────────────────────────

// PHP: test_set_score_single_article
#[tokio::test]
async fn test_set_score_single_article() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let result = ttrss::article::set_score(
                &state,
                &sess,
                &request(&[("ids", json!([1])), ("score", json!(5))]),
            )
            .await;
            assert_eq!(result.get("score").and_then(Value::as_i64), Some(5));

            assert_eq!(score_of(db, 1).await, 5);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_set_score_multiple_articles
#[tokio::test]
async fn test_set_score_multiple_articles() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let result = ttrss::article::set_score(
                &state,
                &sess,
                &request(&[("ids", json!([1, 2, 3])), ("score", json!(10))]),
            )
            .await;
            assert_eq!(result.get("score").and_then(Value::as_i64), Some(10));

            for ref_id in [1i32, 2, 3] {
                assert_eq!(score_of(db, ref_id).await, 10);
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_set_score_zero
#[tokio::test]
async fn test_set_score_zero() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let result = ttrss::article::set_score(
                &state,
                &sess,
                &request(&[("ids", json!([1])), ("score", json!(0))]),
            )
            .await;
            assert_eq!(result.get("score").and_then(Value::as_i64), Some(0));

            assert_eq!(score_of(db, 1).await, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── setArticleTags() ────────────────────────────────────────────────────────

// PHP: test_set_article_tags_new_tags
#[tokio::test]
async fn test_set_article_tags_new_tags() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;

            let result = ttrss::article::set_article_tags(
                &state,
                &sess,
                &request(&[("id", json!(1)), ("tags_str", json!("new-tag-1,new-tag-2"))]),
            )
            .await;
            assert_eq!(
                result.get("tags").and_then(Value::as_array),
                Some(&vec![json!("new-tag-1"), json!("new-tag-2")])
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_set_article_tags_replaces_existing
#[tokio::test]
async fn test_set_article_tags_replaces_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            // Article 1 starts with tag_cache 'rust|open-source'
            let result = ttrss::article::set_article_tags(
                &state,
                &sess,
                &request(&[("id", json!(1)), ("tags_str", json!("replaced-tag"))]),
            )
            .await;
            assert_eq!(
                result.get("tags").and_then(Value::as_array),
                Some(&vec![json!("replaced-tag")])
            );

            // The old tags are gone: exactly 1 tag row remains.
            assert_eq!(tag_row_count(db).await, 1);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_set_article_tags_clears_all
#[tokio::test]
async fn test_set_article_tags_clears_all() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let result = ttrss::article::set_article_tags(
                &state,
                &sess,
                &request(&[("id", json!(1)), ("tags_str", json!(""))]),
            )
            .await;
            assert_eq!(result.get("tags").and_then(Value::as_array), Some(&vec![]));

            assert_eq!(tag_row_count(db).await, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_set_article_tags_trims_whitespace
#[tokio::test]
async fn test_set_article_tags_trims_whitespace() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;

            let result = ttrss::article::set_article_tags(
                &state,
                &sess,
                &request(&[("id", json!(1)), ("tags_str", json!("  tag-a  ,  tag-b  "))]),
            )
            .await;
            // FeedItem_Common::normalize_categories handles trimming
            let tags = result.get("tags").and_then(Value::as_array).unwrap();
            assert!(tags.contains(&json!("tag-a")));
            assert!(tags.contains(&json!("tag-b")));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── printArticleTags() ──────────────────────────────────────────────────────

// PHP: test_print_article_tags_existing
#[tokio::test]
async fn test_print_article_tags_existing() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = common::make_state(&url).await;

            let result = ttrss::article::print_article_tags(state.db.pool(), 1, 1).await;
            assert!(result.get("tags").is_some());
            assert_eq!(result.get("id").and_then(Value::as_i64), Some(1));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_print_article_tags_empty
#[tokio::test]
async fn test_print_article_tags_empty() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = common::make_state(&url).await;

            // Article 11 has tag_cache 'quantum|computing' from seed
            let result = ttrss::article::print_article_tags(state.db.pool(), 1, 11).await;
            assert!(result.get("tags").is_some());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── completeTags() ──────────────────────────────────────────────────────────

// PHP: test_complete_tags_returns_matching_tags
#[tokio::test]
async fn test_complete_tags_returns_matching_tags() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let int_id = int_id_for_ref1(db).await;
            sqlx::query(
                "INSERT INTO ttrss_tags (tag_name, owner_uid, post_int_id) VALUES ($1, 1, $2)",
            )
            .bind("testing-complete")
            .bind(int_id)
            .execute(db.pool())
            .await
            .expect("insert tag");

            let result = ttrss::article::complete_tags(
                &state,
                &sess,
                &request(&[("search", json!("test"))]),
            )
            .await;
            let tags = result.as_array().expect("array");
            assert!(tags.contains(&json!("testing-complete")));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_complete_tags_no_match
#[tokio::test]
async fn test_complete_tags_no_match() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;

            let result = ttrss::article::complete_tags(
                &state,
                &sess,
                &request(&[("search", json!("zzzzzzz-nonexistent"))]),
            )
            .await;
            assert_eq!(result.as_array(), Some(&vec![]));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_complete_tags_case_sensitive
#[tokio::test]
async fn test_complete_tags_case_sensitive() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            let int_id = int_id_for_ref1(db).await;
            sqlx::query(
                "INSERT INTO ttrss_tags (tag_name, owner_uid, post_int_id) VALUES ($1, 1, $2)",
            )
            .bind("TagUppercase")
            .bind(int_id)
            .execute(db.pool())
            .await
            .expect("insert tag");

            let result =
                ttrss::article::complete_tags(&state, &sess, &request(&[("search", json!("T"))]))
                    .await;
            let tags = result.as_array().expect("array");
            assert!(tags.contains(&json!("TagUppercase")));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── assigntolabel() / removefromlabel() ─────────────────────────────────────

// PHP: test_assign_to_label
#[tokio::test]
async fn test_assign_to_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;

            let result = ttrss::article::label_ops(
                &state,
                &sess,
                &request(&[("ids", json!([1])), ("lid", json!(1))]),
                true,
            )
            .await;
            assert!(result.get("labels-for").is_some());
            assert_eq!(
                result.get("message").and_then(Value::as_str),
                Some("UPDATE_COUNTERS")
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_remove_from_label
#[tokio::test]
async fn test_remove_from_label() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = &state.db;

            // First assign a label (article_id references ttrss_entries.id, not int_id)
            sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 1)")
                .execute(db.pool())
                .await
                .expect("assign label");

            let result = ttrss::article::label_ops(
                &state,
                &sess,
                &request(&[("ids", json!([1])), ("lid", json!(1))]),
                false,
            )
            .await;
            assert!(result.get("labels-for").is_some());
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── getmetadatabyid() ───────────────────────────────────────────────────────

// PHP: test_get_metadata_by_id
#[tokio::test]
async fn test_get_metadata_by_id() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = common::make_state(&url).await;

            let result = ttrss::article::metadata_by_id(state.db.pool(), 1, 1).await;
            assert_eq!(
                result.get("link").and_then(Value::as_str),
                Some("https://news.ycombinator.com/item?id=40001")
            );
            assert_eq!(
                result.get("title").and_then(Value::as_str),
                Some("Show HN: I built a distributed task queue in Rust")
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// PHP: test_get_metadata_by_id_nonexistent
#[tokio::test]
async fn test_get_metadata_by_id_nonexistent() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = common::make_state(&url).await;

            let result = ttrss::article::metadata_by_id(state.db.pool(), 1, 99999).await;
            assert_eq!(result.as_array(), Some(&vec![]));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── redirect() ──────────────────────────────────────────────────────────────

// PHP: test_redirect_valid_article
#[tokio::test]
async fn test_redirect_valid_article() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;

            let (status, location, body) =
                ttrss::article::redirect(&state, &sess, &request(&[("id", json!(1))])).await;
            // Handler redirects with Response::redirect()->send() which sends a 302
            // The response body is empty for redirects
            assert_eq!(status.as_u16(), 302);
            assert_eq!(
                location.as_deref(),
                Some("https://news.ycombinator.com/item?id=40001")
            );
            assert!(body.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}
