//! M6b.2b — the DB-backed feed-update pipeline (`ttrss::updates`), mirroring
//! `tt-rss/tests_integration/UpdateRssFeedTest.php`.
//!
//! The Rust `update_rss_feed` takes the feed XML as input (the network fetch is
//! M6c), so these replay the PHP suite's recorded mock feed
//! (`tt-rss/tests_integration/feed.xml` — one Atom article, "x-21may2020")
//! against the versioned schema + baseline + seed fixtures, asserting the
//! permission gate, the article upsert, the feed metadata stamp, and dedup.

mod common;

use sqlx::pool::PoolOptions;
use sqlx::postgres::PgConnectOptions;
use sqlx::Row;
use ttrss::core::db::Db;
use ttrss::updates::update_rss_feed;

/// The recorded mock feed (a single Atom entry, "x-21may2020").
const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// The PHP suite's invalid-XML string.
const BAD_FEED: &str = "<invalid-xml><unclosed>";

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

/// `ttrss_entries` count for a feed (PHP: entries ⋈ user_entries by ref_id).
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

/// The feed's `last_successful_update` (non-null when a successful update ran).
async fn last_successful_update_is_set(db: &Db, feed_id: i32) -> bool {
    sqlx::query_scalar("SELECT last_successful_update IS NOT NULL FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_successful_update")
}

/// The feed's `last_error` (empty after a successful update).
async fn last_error(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT last_error FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("last_error")
}

// PHP: testNonexistentFeedReturnsFalse
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_nonexistent_feed_returns_false(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    assert!(!update_rss_feed(db.pool(), 9999, MOCK_FEED)
        .await
        .expect("update"));
}

// PHP: testReadonlyUserCannotUpdateFeed
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_readonly_user_cannot_update_feed(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let readonly_uid = create_user(&db, 100, -1 /* READONLY */).await;
    let feed_id = create_feed(&db, 100, readonly_uid, "http://example.com/feed1.xml").await;

    assert!(!update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
}

// PHP: testSuccessfulUpdateReturnsTrue
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_successful_update_returns_true(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 101, 0 /* USER */).await;
    let feed_id = create_feed(&db, 101, uid, "http://example.com/feed2.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    assert_eq!(entry_count(&db, feed_id).await, 1);
}

// PHP: testFeedMetadataUpdated
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_feed_metadata_updated(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 102, 0).await;
    let feed_id = create_feed(&db, 102, uid, "http://example.com/feed3.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    assert!(last_successful_update_is_set(&db, feed_id).await);
    assert_eq!(last_error(&db, feed_id).await, "");
}

// PHP: testArticleFieldsStoredCorrectly
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_article_fields_stored_correctly(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 103, 0).await;
    let feed_id = create_feed(&db, 103, uid, "http://example.com/feed4.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));

    let row = sqlx::query(
        "SELECT e.title, e.link, e.updated, e.num_comments \
         FROM ttrss_entries e JOIN ttrss_user_entries ue ON e.id = ue.ref_id \
         WHERE ue.feed_id = $1 LIMIT 1",
    )
    .bind(feed_id)
    .fetch_one(db.pool())
    .await
    .expect("entry row");

    let title: &str = row.get(0);
    let link: &str = row.get(1);
    let updated: Option<chrono::NaiveDateTime> = row.get(2);
    let num_comments: i32 = row.get(3);

    assert_eq!(title, "x-21may2020");
    assert!(
        link.contains("fakecake.org"),
        "link should contain feed domain: {link}"
    );
    assert!(
        link.contains("clouds.png"),
        "link should contain article path: {link}"
    );
    assert!(updated.is_some());
    assert_eq!(num_comments, 0);
}

// PHP: testSecondUpdateDoesNotCreateDuplicate
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_second_update_does_not_create_duplicate(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 104, 0).await;
    let feed_id = create_feed(&db, 104, uid, "http://example.com/feed5.xml").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update #1"));
    assert_eq!(entry_count(&db, feed_id).await, 1);

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update #2"));
    assert_eq!(entry_count(&db, feed_id).await, 1); // still 1, not 2
}

// PHP: testParseErrorReturnsFalse
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_parse_error_returns_false(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 105, 0).await;
    let feed_id = create_feed(&db, 105, uid, "http://example.com/feed6.xml").await;

    assert!(!update_rss_feed(db.pool(), feed_id, BAD_FEED)
        .await
        .expect("update"));
    assert!(!last_error(&db, feed_id).await.is_empty());
}

// ============================================================================
// M6b.2c — article filters (`RSSUtils::eval_article_filters` + apply)
// ============================================================================

/// Filter action ids (`ttrss_filter_actions.id`).
const ACT_FILTER: i32 = 1; // "filter"
const ACT_CATCHUP: i32 = 2; // "catchup"
const ACT_TAG: i32 = 4; // "tag"
const ACT_SCORE: i32 = 6; // "score"
const ACT_LABEL: i32 = 7; // "label"
const ACT_IGNORE_TAG: i32 = 10; // "ignore-tag"

/// The recorded mock article's title, as a PCRE fragment (no metacharacters).
const TITLE_RULE: &str = "21may2020";

/// Insert an enabled filter for `owner_uid` with a single unscoped rule
/// matching `pattern` against `filter_type` (1=title, 2=content, ...).
async fn create_filter(db: &Db, owner_uid: i32, pattern: &str, filter_type: i32) -> i32 {
    let filter_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, title, enabled) \
         VALUES ($1, 'test filter', true) RETURNING id",
    )
    .bind(owner_uid)
    .fetch_one(db.pool())
    .await
    .expect("insert filter");
    sqlx::query(
        "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) \
         VALUES ($1, $2, $3)",
    )
    .bind(filter_id)
    .bind(pattern)
    .bind(filter_type)
    .execute(db.pool())
    .await
    .expect("insert rule");
    filter_id
}

/// Attach an action (by `ttrss_filter_actions.id`) to a filter.
async fn add_filter_action(db: &Db, filter_id: i32, action_id: i32, param: &str) {
    sqlx::query(
        "INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) \
         VALUES ($1, $2, $3)",
    )
    .bind(filter_id)
    .bind(action_id)
    .bind(param)
    .execute(db.pool())
    .await
    .expect("insert action");
}

/// The user entry's `(unread, marked)` for a feed's single article, if any.
async fn user_entry_flags(db: &Db, feed_id: i32) -> Option<(bool, bool)> {
    sqlx::query(
        "SELECT unread, marked FROM ttrss_user_entries ue \
         JOIN ttrss_entries e ON e.id = ue.ref_id WHERE ue.feed_id = $1",
    )
    .bind(feed_id)
    .fetch_optional(db.pool())
    .await
    .expect("user entry flags")
    .map(|r| (r.get::<bool, _>(0), r.get::<bool, _>(1)))
}

/// The tag names on a feed's single article.
async fn article_tags(db: &Db, feed_id: i32) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT t.tag_name FROM ttrss_tags t \
         JOIN ttrss_user_entries ue ON ue.int_id = t.post_int_id \
         WHERE ue.feed_id = $1 ORDER BY t.tag_name",
    )
    .bind(feed_id)
    .fetch_all(db.pool())
    .await
    .expect("article tags")
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_filter_action_drops_article(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 200, 0).await;
    let feed_id = create_feed(&db, 200, uid, "http://example.com/f1.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1 /* title */).await;
    add_filter_action(&db, filter_id, ACT_FILTER, "").await;
    let base_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries")
        .fetch_one(db.pool())
        .await
        .expect("base count");

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    // The base entry is created, but the `filter` action drops it (no user entry).
    assert_eq!(
        entry_count(&db, feed_id).await,
        0,
        "article should be filtered out (no user entry)"
    );
    let base_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_entries")
        .fetch_one(db.pool())
        .await
        .expect("base count");
    assert_eq!(
        base_after,
        base_before + 1,
        "the base entry is still created"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_score_action_marks_starred(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 201, 0).await;
    let feed_id = create_feed(&db, 201, uid, "http://example.com/f2.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_SCORE, "2000").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let (unread, marked) = user_entry_flags(&db, feed_id).await.expect("flags");
    assert!(marked, "score 2000 (>1000) should mark the article starred");
    assert!(unread, "score 2000 is >= -500, so the article stays unread");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_score_action_below_read_threshold(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 202, 0).await;
    let feed_id = create_feed(&db, 202, uid, "http://example.com/f3.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_SCORE, "-1000").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let (unread, marked) = user_entry_flags(&db, feed_id).await.expect("flags");
    assert!(!unread, "score -1000 (<-500) should mark the article read");
    assert!(!marked, "score -1000 should not mark the article starred");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_catchup_action_marks_read(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 203, 0).await;
    let feed_id = create_feed(&db, 203, uid, "http://example.com/f4.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_CATCHUP, "").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let (unread, _marked) = user_entry_flags(&db, feed_id).await.expect("flags");
    assert!(!unread, "a `catchup` action should mark the article read");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_tag_action_adds_tags(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 204, 0).await;
    let feed_id = create_feed(&db, 204, uid, "http://example.com/f5.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_TAG, "rust, tiny-rss").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let tags = article_tags(&db, feed_id).await;
    assert!(tags.contains(&"rust".to_string()), "tags: {tags:?}");
    assert!(tags.contains(&"tiny-rss".to_string()), "tags: {tags:?}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_ignore_tag_removes_tag(pool: PoolOptions<sqlx::Postgres>, options: PgConnectOptions) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 205, 0).await;
    let feed_id = create_feed(&db, 205, uid, "http://example.com/f6.xml").await;
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_TAG, "rust, spam").await;
    add_filter_action(&db, filter_id, ACT_IGNORE_TAG, "spam").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let tags = article_tags(&db, feed_id).await;
    assert!(tags.contains(&"rust".to_string()), "tags: {tags:?}");
    assert!(
        !tags.contains(&"spam".to_string()),
        "the `ignore-tag` action should remove the tag: {tags:?}"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_blacklisted_tag_removed(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 206, 0).await;
    let feed_id = create_feed(&db, 206, uid, "http://example.com/f7.xml").await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, value) \
         VALUES ($1, 'BLACKLISTED_TAGS', 'spam')",
    )
    .bind(uid)
    .execute(db.pool())
    .await
    .expect("set blacklist");
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_TAG, "rust, spam").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let tags = article_tags(&db, feed_id).await;
    assert!(tags.contains(&"rust".to_string()), "tags: {tags:?}");
    assert!(
        !tags.contains(&"spam".to_string()),
        "a blacklisted tag should be removed: {tags:?}"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_label_action_assigns_label(
    pool: PoolOptions<sqlx::Postgres>,
    options: PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let uid = create_user(&db, 207, 0).await;
    let feed_id = create_feed(&db, 207, uid, "http://example.com/f8.xml").await;
    let label_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_labels2 (owner_uid, caption) VALUES ($1, 'My Label') RETURNING id",
    )
    .bind(uid)
    .fetch_one(db.pool())
    .await
    .expect("create label");
    let filter_id = create_filter(&db, uid, TITLE_RULE, 1).await;
    add_filter_action(&db, filter_id, ACT_LABEL, "My Label").await;

    assert!(update_rss_feed(db.pool(), feed_id, MOCK_FEED)
        .await
        .expect("update"));
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_labels2 ul \
         JOIN ttrss_entries e ON e.id = ul.article_id \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         WHERE ue.feed_id = $1 AND ul.label_id = $2",
    )
    .bind(feed_id)
    .bind(label_id)
    .fetch_one(db.pool())
    .await
    .expect("label count");
    assert_eq!(
        n, 1,
        "the `label` action should assign the label to the article"
    );
}
