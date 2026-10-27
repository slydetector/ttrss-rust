//! DB tests for the `getHeadlines` params the port once dropped (the
//! "lostboys" gap B5, `docs/18-php-port-gap-scan.md`).
//!
//! PHP's `API::getHeadlines` reads 19 params; the port read 15. These pin the
//! three with real behavior:
//! - `search` — threaded to `_search_to_sql` (a client sending `search=` must
//!   get a filtered list, not the unfiltered one);
//! - `force_update` — a pre-read forced refresh with a 120 s flood control:
//!   a real feed stale by more than 120 s is fetched synchronously (the
//!   `$no_cache` long-timeout variant); a recent feed or a cache-images feed
//!   gets its `last_updated`/`last_update_started` stamps reset to the epoch
//!   (no synchronous fetch);
//! - `check_first_id` — the changed-result-set signal for incremental clients
//!   (`offset > 0` + a stale `check_first_id` → an empty list).
//!
//! (`filter` is read by the PHP and then dropped — a dead param in the frozen
//! reference, `_api_get_headlines` never uses it — so it is ignored here.)

use crate::common;

use common::{Canned, MockServer};
use serde_json::{Map, Value, json};
use sqlx::AssertSqlSafe;
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::api;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;

const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// Fixtures + a fresh session carrying `uid = 1` + the per-user API gate.
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set(
        "pwd_hash",
        PValue::Bytes(crate::common::ADMIN_PWD_HASH.as_bytes().to_vec()),
    );
    data.set("access_level", PValue::Int(10));
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

/// `getheadlines` → the response item ids (in order).
async fn ids(state: &AppState, sess: &Sess, pairs: &[(&str, Value)]) -> Vec<i32> {
    let (status, content) =
        api::before_and_dispatch(state, sess, "getheadlines", &request(pairs), None, false).await;
    assert_eq!(status, 0);
    content
        .as_array()
        .expect("array response")
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_i64).map(|v| v as i32))
        .collect()
}

/// Feed 5 (cat 1) + entries 50 (older) / 51 (newer) owned by uid 1.
async fn add_search_fixtures(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url)
         VALUES (5, 1, 'Search feed', 1, 'https://example.com/feed', 60, 0, 'https://example.com')",
    )
    .execute(pool)
    .await
    .expect("feed insert");
    // `date_entered` within the last hour — the first-id check (PHP and port)
    // only scans `date_entered >= NOW() - INTERVAL '1 hour'`.
    for (id, title, entered) in [
        (50i32, "Alpha zebra story", "NOW() - INTERVAL '30 minutes'"),
        (51i32, "Beta quokka notes", "NOW() - INTERVAL '20 minutes'"),
    ] {
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO ttrss_entries
               (id, title, guid, link, updated, content, content_hash, date_entered, date_updated,
                tsvector_combined, lang)
             VALUES ({id}, '{title}', 'tag:example.com,2026:{id}', 'https://example.com/{id}',
                     {entered}, 'body', 'SHA1:0000000000000000000000000000000000000000',
                     {entered}, {entered}, to_tsvector('simple', '{title}' || ' body'), 'en')"
        )))
        .execute(pool)
        .await
        .expect("entry insert");
        sqlx::query(
            "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
             VALUES ($1, $2, 5, 1, true, false, '', '')",
        )
        .bind(id)
        .bind(format!("uuid-{id}"))
        .execute(pool)
        .await
        .expect("user entry insert");
    }
}

#[tokio::test]
async fn test_search_filters_headlines() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let pool = state.db.pool();
            // Plain FTS words need a search language (the API path leaves
            // `search_language` at "" → the `DEFAULT_SEARCH_LANGUAGE` pref).
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
             VALUES (1, 'DEFAULT_SEARCH_LANGUAGE', NULL, 'simple')",
            )
            .execute(pool)
            .await
            .expect("language pref");
            add_search_fixtures(pool).await;

            // No search → both articles (newest first).
            assert_eq!(
                ids(&state, &sess, &[("feed_id", json!(5))]).await,
                vec![51, 50]
            );
            // `search=zebra` → only the zebra article (the port once ignored it).
            assert_eq!(
                ids(
                    &state,
                    &sess,
                    &[("feed_id", json!(5)), ("search", json!("zebra"))]
                )
                .await,
                vec![50],
                "search must filter the headline list"
            );
            // `search=quokka` → only the quokka article.
            assert_eq!(
                ids(
                    &state,
                    &sess,
                    &[("feed_id", json!(5)), ("search", json!("quokka"))]
                )
                .await,
                vec![51],
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_check_first_id_changed_result_set() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            add_search_fixtures(state.db.pool()).await;

            // `offset > 0` + a matching `check_first_id` (the current top id, 51)
            // → the page is served.
            assert_eq!(
                ids(
                    &state,
                    &sess,
                    &[
                        ("feed_id", json!(5)),
                        ("skip", json!(1)),
                        ("check_first_id", json!(51))
                    ]
                )
                .await,
                vec![50],
            );
            // A stale `check_first_id` → the result set changed → an empty list
            // (PHP returns `[-1, …]`; the API renders it as no headlines).
            assert_eq!(
                ids(
                    &state,
                    &sess,
                    &[
                        ("feed_id", json!(5)),
                        ("skip", json!(1)),
                        ("check_first_id", json!(50))
                    ]
                )
                .await,
                Vec::<i32>::new(),
                "changed result set must signal via an empty list"
            );
            // `offset = 0` → the check is skipped (PHP's `offset > 0` gate).
            assert_eq!(
                ids(
                    &state,
                    &sess,
                    &[("feed_id", json!(5)), ("check_first_id", json!(50))]
                )
                .await,
                vec![51, 50],
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A loopback mock that serves `MOCK_FEED` (one Atom entry) with `status`.
fn feed_mock(status: u16) -> MockServer {
    MockServer::new(vec![Canned {
        status,
        reason: if status == 200 {
            "OK"
        } else {
            "Internal Server Error"
        },
        headers: vec![("content-type", "application/atom+xml".to_string())],
        body: MOCK_FEED.as_bytes().to_vec(),
    }])
}

/// Feed 5 pointing at `feed_url`, with the given `last_updated` + `cache_images`.
async fn add_force_feed(
    pool: &sqlx::PgPool,
    feed_url: &str,
    last_updated: &str,
    cache_images: bool,
) {
    sqlx::query(
        "INSERT INTO ttrss_feeds
           (id, owner_uid, title, cat_id, feed_url, update_interval, purge_interval, site_url,
            last_updated, cache_images)
         VALUES (5, 1, 'Force feed', 1, $1, 60, 0, 'https://example.com', $2::timestamp, $3)",
    )
    .bind(feed_url)
    .bind(last_updated)
    .bind(cache_images)
    .execute(pool)
    .await
    .expect("feed insert");
}

/// The feed-5 `last_updated` / `last_update_started` / `last_error`.
async fn stamps(pool: &sqlx::PgPool) -> (String, String, Option<String>) {
    sqlx::query_as(
        "SELECT last_updated::text, last_update_started::text, last_error FROM ttrss_feeds WHERE id = 5",
    )
    .fetch_one(pool)
    .await
    .expect("stamp query")
}

/// A stale, non-cache-images feed is fetched synchronously (the `$no_cache`
/// variant) — the mock's entry is imported and `last_updated` re-stamped.
#[tokio::test]
async fn test_force_update_stale_feed_fetches() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;
        let pool = state.db.pool();
        let mock = feed_mock(200);
        add_force_feed(pool, &mock.url("/feed"), "2020-01-01 00:00:00", false).await;

        ids(&state, &sess, &[("feed_id", json!(5)), ("force_update", json!(true))]).await;

        // The synchronous fetch imported the mock feed's single article.
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ttrss_entries e
             WHERE e.id IN (SELECT ref_id FROM ttrss_user_entries WHERE feed_id = 5 AND owner_uid = 1)",
        )
        .fetch_one(pool)
        .await
        .expect("entry count");
        assert_eq!(n, 1, "the forced fetch must import the article");
        // And re-stamped `last_updated` to ~now (was 2 hours ago).
        let fresh: bool = sqlx::query_scalar(
            "SELECT last_updated > NOW() - INTERVAL '1 minute' FROM ttrss_feeds WHERE id = 5",
        )
        .fetch_one(pool)
        .await
        .expect("freshness query");
        assert!(fresh, "last_updated re-stamped to now");
        Ok(())
    }))
    .await
    .unwrap();
}

/// A recent feed (within the 120 s flood control) is NOT fetched — its stamps
/// are reset to the epoch so the updater picks it up. A 500 mock proves no
/// fetch was attempted (a failed fetch would stamp `last_error`).
#[tokio::test]
async fn test_force_update_recent_feed_resets_stamps() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let pool = state.db.pool();
            let mock = feed_mock(500);
            add_force_feed(pool, &mock.url("/feed"), "2099-01-01 00:00:00", false).await;

            ids(
                &state,
                &sess,
                &[("feed_id", json!(5)), ("force_update", json!(true))],
            )
            .await;

            let (lu, lus, err) = stamps(pool).await;
            assert!(lu.starts_with("1970-01-01"), "last_updated reset, got {lu}");
            assert!(
                lus.starts_with("1970-01-01"),
                "last_update_started reset, got {lus}"
            );
            assert!(
                err.as_deref().unwrap_or("").is_empty(),
                "no fetch attempted within the flood control: {err:?}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A stale cache-images feed (favicon-only) is NOT fetched synchronously
/// either — same stamp reset.
#[tokio::test]
async fn test_force_update_cache_images_feed_resets_stamps() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let pool = state.db.pool();
            let mock = feed_mock(500);
            add_force_feed(pool, &mock.url("/feed"), "2020-01-01 00:00:00", true).await;

            ids(
                &state,
                &sess,
                &[("feed_id", json!(5)), ("force_update", json!(true))],
            )
            .await;

            let (lu, lus, err) = stamps(pool).await;
            assert!(lu.starts_with("1970-01-01"), "last_updated reset, got {lu}");
            assert!(
                lus.starts_with("1970-01-01"),
                "last_update_started reset, got {lus}"
            );
            assert!(
                err.as_deref().unwrap_or("").is_empty(),
                "cache-images feeds are not fetched synchronously: {err:?}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Web-RPC session (`op=Feeds&method=view` gate: auth + CSRF token).
fn view_sess() -> Sess {
    let mut d = Session::new();
    d.set("uid", PValue::Int(1));
    d.set(
        "pwd_hash",
        PValue::Bytes(crate::common::ADMIN_PWD_HASH.as_bytes().to_vec()),
    );
    d.set("access_level", PValue::Int(10));
    d.set(
        "access_level",
        PValue::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64),
    );
    d.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    d.set("name", PValue::Bytes(b"admin".to_vec()));
    d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let s = Sess::new("cdm-view".into());
    s.replace("cdm-view".into(), d);
    s
}

/// `op=Feeds&method=view&feed=5` → the response's `headlines.content` rows.
async fn view_rows(state: &AppState) -> Vec<Value> {
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/backend.php")
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(axum::body::Body::from(
            "op=Feeds&method=view&feed=5&csrf_token=tok".to_string(),
        ))
        .unwrap();
    let res = ttrss::rpc::handler(
        axum::extract::State(state.clone()),
        axum::extract::Extension(view_sess()),
        req,
    )
    .await;
    let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v.pointer("/headlines/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `cdm_excerpt` (Feeds.php:329-338): the collapsed-CDM close button, the
/// optional preview span, and the key's absence when `CDM_EXPANDED` is on.
/// Pinned on the WEB RPC path — the API path builds its own row shape and
/// (like PHP's `API::_api_get_headlines`) has no `cdm_excerpt` key.
#[tokio::test]
async fn test_cdm_excerpt_states() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, _sess) = setup(&url).await;
            let pool = state.db.pool();
            add_search_fixtures(pool).await;

            // Default (`CDM_EXPANDED` = true, auto-expand): no key at all.
            let row = &view_rows(&state).await[0];
            assert!(
                row.get("cdm_excerpt").is_none(),
                "CDM_EXPANDED on (default) → no cdm_excerpt key"
            );

            // `CDM_EXPANDED` off → the close-button span; `SHOW_CONTENT_PREVIEW`
            // defaults on (PHP `Prefs.php:74`), so the excerpt span is present
            // too (the fixture entry's content is "body").
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
                 VALUES (1, 'CDM_EXPANDED', NULL, 'false')",
            )
            .execute(pool)
            .await
            .expect("expanded pref off");
            let row = &view_rows(&state).await[0];
            assert_eq!(
                row["cdm_excerpt"].as_str(),
                Some(
                    "<span class='collapse'><i class='material-icons' \
                     onclick='return Article.cdmUnsetActive(event)' \
                     title=\"Collapse article\">remove_circle</i></span>\
                     <span class='excerpt'>&mdash; body</span>"
                ),
                "CDM_EXPANDED off → close button + default-on excerpt"
            );

            // `SHOW_CONTENT_PREVIEW` off → the button only.
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
                 VALUES (1, 'SHOW_CONTENT_PREVIEW', NULL, 'false')",
            )
            .execute(pool)
            .await
            .expect("preview pref off");
            let row = &view_rows(&state).await[0];
            assert_eq!(
                row["cdm_excerpt"].as_str(),
                Some(
                    "<span class='collapse'><i class='material-icons' \
                     onclick='return Article.cdmUnsetActive(event)' \
                     title=\"Collapse article\">remove_circle</i></span>"
                ),
                "preview off → close button only"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
