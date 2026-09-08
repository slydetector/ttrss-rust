//! DB tests for the search scope — the `_search_to_sql` fragment spliced
//! into the headline query (with its dry-run precheck) and into
//! `Feeds::_catchup`.
//!
//! Rust-only: the frozen PHP suite has no `_search_to_sql` or catchup-search
//! coverage, so there is no 1:1 parity mapping. The fragment *text* itself
//! is oracle-verified in `tests/search_to_sql.rs`; these tests verify the
//! splicing behavior (which rows a search actually selects/marks).
//!
//! NOTE: with the default (empty) search language, plain FTS words generate
//! `to_tsquery('', …)` which Postgres rejects at parse time ("invalid name
//! syntax") — so the dry-run precheck degrades the filter to `false` and
//! reports "Incorrect search syntax". This is the actual tt-rss behavior
//! (verified in the oracle app); FTS row filtering requires a search
//! language (e.g. "english"), as the tests below do.
//!
//! Fixtures: feeds 1-4 (categories 1-2), labels 1-2, articles 1-11 with
//! `tsvector_combined` seeded (feed 1: refs 1,2 unread + 3 read&marked;
//! feed 2: refs 4-6 unread; feed 3: ref 7 unread + 8 read&marked;
//! feed 4: refs 9,10 unread + 11 read).

mod common;

use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::headlines::{get_headlines_query, HeadlinesParams};
use ttrss_server::session::Sess;
use ttrss_server::AppState;

fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8282".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
    }
}

async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess, Db) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState {
        config: test_config(),
        db: db.clone(),
    };
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess, db)
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

fn params(feed: &str, search: &str, search_language: &str) -> HeadlinesParams {
    let mut p = HeadlinesParams::new(feed, 1);
    p.search = search.to_string();
    p.search_language = search_language.to_string();
    p
}

/// The ref ids a headline query returns for `feed` + `search`.
async fn head_ids(db: &Db, feed: &str, search: &str, lang: &str) -> Vec<i32> {
    let q = get_headlines_query(db, &params(feed, search, lang)).await;
    q.article_rows.iter().map(|r| r.get::<i32, _>(0)).collect()
}

/// Read state of every owner-1 entry, ordered by ref id.
struct Entry {
    ref_id: i32,
    unread: bool,
    last_read_set: bool,
}

async fn entries(db: &Db) -> Vec<Entry> {
    let rows: Vec<(i32, bool, bool)> =
        sqlx::query_as("SELECT ref_id, unread, (last_read IS NOT NULL)
                        FROM ttrss_user_entries WHERE owner_uid = 1 ORDER BY ref_id")
            .fetch_all(db.pool())
            .await
            .expect("entries");
    rows.into_iter()
        .map(|(ref_id, unread, last_read_set)| Entry { ref_id, unread, last_read_set })
        .collect()
}
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn headline_fts_filters_rows(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (_state, _sess, db) = setup(pool, options).await;
    let lang = "english";

    // Bare words → FTS (stemmed, english config). Feed 1 = HN (refs 1-3),
    // 2 = Register (4-6), 3 = Ars (7-8), 4 = MIT Tech Review (9-11).
    assert_eq!(head_ids(&db, "1", "task", lang).await, vec![1]);
    assert_eq!(head_ids(&db, "1", "performance", lang).await, vec![2]);
    assert_eq!(head_ids(&db, "2", "outage", lang).await, vec![5]);
    assert_eq!(head_ids(&db, "3", "kubernetes", lang).await, vec![8]);
    assert_eq!(head_ids(&db, "4", "quantum", lang).await, vec![11]);
    // Stemming: singular query matches the plural in the content.
    assert_eq!(head_ids(&db, "1", "improvement", lang).await, vec![2]);
    // No match inside the feed view (the word lives in another feed).
    assert_eq!(head_ids(&db, "1", "outage", lang).await, Vec::<i32>::new());
    // FTS operator: union of the two word sets within the feed.
    assert_eq!(
        head_ids(&db, "2", "autonomous|outage", lang).await,
        vec![4, 5]
    );
    // "postgres" does not match "postgresql" (article 2, feed 1).
    assert_eq!(head_ids(&db, "1", "postgres", lang).await, Vec::<i32>::new());
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn headline_fts_empty_language_is_unexecutable(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (_state, _sess, db) = setup(pool, options).await;

    // Default search language "" → to_tsquery('', …) → parse error in the
    // dry run → filter degrades to `false`, no rows, override message.
    let q = get_headlines_query(&db, &params("1", "task", "")).await;
    assert!(q.article_rows.is_empty());
    assert_eq!(q.query_error_override, "Incorrect search syntax: task.");
    assert_eq!(q.search_words, vec!["task".to_string()]);
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn headline_title_like_filters(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (_state, _sess, db) = setup(pool, options).await;
    let lang = "english";

    // `title:` is a case-insensitive substring match on the title — no FTS
    // involved, so it works with any (or no) search language.
    assert_eq!(head_ids(&db, "1", "title:postgres", lang).await, vec![2]); // "PostgreSQL"
    assert_eq!(head_ids(&db, "3", "title:Kubernetes", lang).await, vec![8]);
    assert_eq!(head_ids(&db, "2", "title:outage", lang).await, vec![5]);
    // Negation: everything except titles containing the word.
    assert_eq!(head_ids(&db, "2", "-title:outage", lang).await, vec![4, 6]);
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn headline_star_search(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (_state, _sess, db) = setup(pool, options).await;
    let lang = "english";

    // Only ref 3 is marked in feed 1; only ref 8 in feed 3.
    assert_eq!(head_ids(&db, "1", "star:true", lang).await, vec![3]);
    assert_eq!(head_ids(&db, "3", "star:true", lang).await, vec![8]);
    assert_eq!(head_ids(&db, "1", "star:false", lang).await, vec![1, 2]);
    // Combined: FTS + star in the same query.
    assert_eq!(
        head_ids(&db, "1", "task|webassembly star:true", lang).await,
        vec![3]
    );
    assert_eq!(
        head_ids(&db, "1", "task|webassembly star:false", lang).await,
        vec![1]
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn headline_dry_run_errors(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (_state, _sess, db) = setup(pool, options).await;

    // `note:` resolves against `ttrss_user_entries.note` (valid SQL); the
    // fixture has no notes until we add one.
    sqlx::query("UPDATE ttrss_user_entries SET note = 'important abc' WHERE ref_id = 1")
        .execute(db.pool())
        .await
        .expect("set note");
    assert_eq!(head_ids(&db, "1", "note:abc", "").await, vec![1]);
    assert_eq!(head_ids(&db, "1", "-note:abc", "").await, vec![2, 3]);

    // Unrecognized `@date` values emit a fragment that errors at execution;
    // the words carry the specific diagnostic.
    let q = get_headlines_query(&db, &params("1", "@bogus-word", "")).await;
    assert!(q.article_rows.is_empty());
    assert_eq!(
        q.query_error_override,
        // (double period: the word carries its own, the template adds one)
        "Incorrect search syntax: The date keyword \"bogus-word\" was not recognized.."
    );

    // A valid search leaves the override empty.
    let q = get_headlines_query(&db, &params("1", "task", "english")).await;
    assert!(q.query_error_override.is_empty());
    assert_eq!(q.search_words, vec!["task".to_string()]);
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn catchup_scoped_to_search(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess, db) = setup(pool, options).await;
    let before = entries(&db).await;

    // `RPC::catchupFeed` with a search: only the unread entries matching
    // "task" (article 1 in feed 1) are marked read.
    let result = ttrss_server::feeds::catchup_feed(
        &state,
        &sess,
        &request(&[
            ("feed_id", json!("1")),
            ("search_query", json!("task")),
            ("search_lang", json!("english")),
        ]),
    )
    .await;
    assert!(result.get("counters").is_some(), "got {result}");

    let after = entries(&db).await;
    for (b, a) in before.iter().zip(&after) {
        if b.ref_id == 1 {
            assert!(!a.unread && a.last_read_set, "ref 1 should be read");
        } else {
            assert_eq!(a.unread, b.unread, "ref {} untouched", b.ref_id);
            assert_eq!(a.last_read_set, b.last_read_set);
        }
    }
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn catchup_search_with_mode_window(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess, db) = setup(pool, options).await;

    // A FRESH unread "task" article in feed 1 (date_entered = NOW()).
    sqlx::query(
        "INSERT INTO ttrss_entries (id, title, guid, link, updated, content,
            content_hash, date_entered, date_updated, tsvector_combined)
         VALUES (12, 'Fresh task queue article', 'tag:test:12',
            'https://example.com/12', NOW(), '<p>task</p>', 'h',
            NOW(), NOW(), to_tsvector('english', 'Fresh task queue article'))",
    )
    .execute(db.pool())
    .await
    .expect("insert fresh entry");
    sqlx::query(
        "INSERT INTO ttrss_user_entries
            (ref_id, uuid, feed_id, owner_uid, unread, marked, tag_cache, label_cache)
         VALUES (12, 'uuid-fresh-001', 1, 1, true, false, '', '')",
    )
    .execute(db.pool())
    .await
    .expect("insert fresh user entry");

    // mode=1day: only entries OLDER than a day are in scope → the old
    // article 1 is read, the fresh 12 is not.
    ttrss_server::feeds::catchup_feed(
        &state,
        &sess,
        &request(&[
            ("feed_id", json!("1")),
            ("mode", json!("1day")),
            ("search_query", json!("task")),
            ("search_lang", json!("english")),
        ]),
    )
    .await;

    let after = entries(&db).await;
    let by_id = |id: i32| after.iter().find(|e| e.ref_id == id).unwrap();
    assert!(!by_id(1).unread && by_id(1).last_read_set, "old ref 1 read");
    assert!(by_id(12).unread, "fresh ref 12 excluded by the 1day window");
    assert!(by_id(2).unread, "ref 2 untouched");
}
