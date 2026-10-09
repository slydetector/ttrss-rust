//! Rust-only regression test for the `Feeds::view` label-cache side effect.
//!
//! PHP's `_format_headlines_list` (Feeds.php) renders each article's labels
//! and, for an article with `num_labels > 0` but an empty `label_cache`, falls
//! back to `Article::_get_labels($id)` — the **hard path**, which ALSO writes
//! the result back to `ttrss_user_entries.label_cache` (`Labels::update_cache`).
//! That write is load-bearing: a later `Article::_labels_of` (in the mutation
//! handlers, used by the SPA's post-marking `getAllCounters` refresh) derives
//! the affected labels from `label_cache`, so the per-label counters update
//! immediately instead of waiting for the next poll.
//!
//! The port originally rendered the labels in the empty-cache branch *without*
//! that write (a local `get_labels_fallback`), so the SPA's label unread badge
//! stuck while scrolling. The fix routes the branch through the canonical
//! `labels::get_labels_json` (the faithful `_get_labels` port that writes the
//! cache).
//!
//! The frozen PHP suite only exercises `_get_headlines` (the query builder),
//! not this side effect, so this Rust-only test pins the corrected behavior:
//! after `view` renders a labeled, empty-cache article, its `label_cache` is
//! populated.

mod common;

use serde_json::{Map, Value, json};
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::headlines::view;
use ttrss::labels::label_to_feed_id;
use ttrss::session::Sess;

const SESSION_UID: i64 = 1;

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
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

/// The `label_cache` text for one user entry, or `None` if the row is absent.
async fn label_cache(pool: &sqlx::Pool<sqlx::Postgres>, article_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT label_cache FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
    )
    .bind(article_id)
    .bind(SESSION_UID)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn test_view_populates_label_cache() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let p = state.db.pool();

            // Link label 1 to article 1 (in feed 1) so article 1 has `num_labels = 1`
            // while its `label_cache` is still empty (as the seed leaves it).
            sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES (1, 1)")
                .execute(p)
                .await
                .unwrap();

            // Precondition: the seed leaves the cache empty.
            assert_eq!(
                label_cache(p, 1).await.as_deref(),
                Some(""),
                "article 1 starts with an empty label_cache"
            );

            let expected_feed_id = label_to_feed_id(1);

            // Render feed 1 (which contains article 1) via the SPA `view` handler.
            let extra: Map<String, Value> = Map::from_iter(vec![("feed".to_string(), json!("1"))]);
            let reply = view(&state, &sess, &extra).await;

            // The article's labels are present in the response…
            let labels = reply
                .get("headlines")
                .and_then(|h| h.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|arts| arts.iter().find(|a| a.get("id") == Some(&json!(1))))
                .and_then(|a| a.get("labels"))
                .and_then(|l| l.as_array())
                .expect("article 1 in the view has a labels array");
            assert!(
                labels.iter().any(|l| l[0] == json!(expected_feed_id)),
                "labels array contains the virtual feed id for label 1: {labels:?}"
            );

            // …AND the hard path wrote the cache back (the `_get_labels` side effect).
            let cached = label_cache(p, 1).await.unwrap_or_default();
            assert!(
                !cached.is_empty(),
                "label_cache is populated after view: {cached:?}"
            );
            assert!(
                cached.contains(&expected_feed_id.to_string()),
                "cache records label 1's virtual feed id: {cached:?}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
