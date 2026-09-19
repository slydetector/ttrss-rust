//! DB tests for `Pref_Filters::testFilterDo` (the "Test Filter" dialog, M5)
//! + the request-parse `[]` normalization regression.
//!
//! Rust-only: the frozen PHP suite has no `testFilterDo` test; the pure
//! evaluation engine is covered 1:1 by the `ttrss::core::filters` unit tests
//! (ported from `tests_functional/RSSUtilsTest.php`). These exercise the
//! handler end to end on the versioned `schema` + `baseline` + `seed`
//! fixtures. Owner 1 is the test session's uid.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;

fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        db_max_connections: 10,
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_max_jobs: 2,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1`.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Insert an entry + an unread user entry for owner 1 (2026-09-01, newer
/// than the seed entries' May dates).
async fn add_entry(
    db: &Db,
    id: i32,
    title: &str,
    content: &str,
    author: &str,
    tag_cache: &str,
    feed_id: i32,
) {
    sqlx::query(
        "INSERT INTO ttrss_entries (id, title, guid, link, updated, content, \
         content_hash, date_entered, date_updated, author) VALUES \
         ($1, $2, $3, 'https://example.com/a', '2026-09-01 10:00:00', $4, \
          'SHA1:extra00000000000000000000000000000000000', \
          '2026-09-01 10:00:00', '2026-09-01 10:00:00', $5)",
    )
    .bind(id)
    .bind(title)
    .bind(format!("tag:example.com,2026:tft-{id}"))
    .bind(content)
    .bind(author)
    .execute(db.pool())
    .await
    .expect("setup: entry");
    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, unread, \
         marked, tag_cache, label_cache) VALUES ($1, $2, $3, 1, true, false, $4, '')",
    )
    .bind(id)
    .bind(format!("uuid-ue-{id}"))
    .bind(feed_id)
    .bind(tag_cache)
    .execute(db.pool())
    .await
    .expect("setup: user entry");
}

/// A `rule[]` JSON param value: an array of JSON-object strings (the shape
/// the parse boundary + handler expect).
fn rule_param(rules: &[Value]) -> Value {
    json!(rules.iter().map(|r| r.to_string()).collect::<Vec<_>>())
}

/// Strip HTML tags (element markup only; text content is kept) — the
/// visible text the SPA sees.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The item(s) whose visible title contains `title_substring`, or `None`
/// if none. The handler's `highlight_words_str` DOM round-trip wraps
/// highlighted titles in an outer `<span>`, so match on the visible text.
fn items_for<'a>(result: &'a Value, title_substring: &str) -> Option<Vec<&'a Value>> {
    let found: Vec<&Value> = result["items"]
        .as_array()?
        .iter()
        .filter(|it| plain(it["title"].as_str().unwrap_or("")).contains(title_substring))
        .collect();
    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

/// The handler's scope query, as a row count (the seed fixtures add their
/// own entries; assert against this + the test's own inserts).
async fn count_entries(db: &Db, where_clause: &str) -> i64 {
    let sql = format!(
        "SELECT count(*) FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         LEFT JOIN ttrss_feeds f ON f.id = ue.feed_id \
         LEFT JOIN ttrss_feed_categories fc ON fc.id = f.cat_id \
         WHERE ue.owner_uid = 1 AND {where_clause}"
    );
    sqlx::query_scalar(&sql)
        .fetch_one(db.pool())
        .await
        .expect("entry count")
}

/// `test_filter_do` without the PHP-fatal `Err` path (the 200-path tests
/// expect no empty highlight word).
async fn do_test(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    ttrss::filters::test_filter_do(state, sess, extra)
        .await
        .expect("no PHP fatal (empty highlight word)")
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_title_match(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(
        db,
        101,
        "A needle article",
        "<p>content a</p>",
        "alice",
        "",
        1,
    )
    .await;
    add_entry(db, 102, "Another one", "<p>content b</p>", "bob", "", 2).await;

    // Title rule, all feeds (`feed_id: 0`).
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("offset", json!("0")),
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "needle", "filter_type": 1, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;

    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "true").await
    );
    let items = items_for(&result, "needle").expect("matching item");
    assert_eq!(items.len(), 1);
    let it = &items[0];
    // The title is highlighted (case preserved, `<span class="highlight">`)
    // and wrapped in the outer `<span>` from the DOM round-trip.
    assert_eq!(
        it["title"],
        "<span>A <span class=\"highlight\">needle</span> article</span>"
    );
    // A title rule's preview is `&mdash;`.
    assert_eq!(it["content_preview"], "&mdash;");
    assert_eq!(it["feed_title"], "Hacker News");
    assert_eq!(it["date"], "2026-09-01 10:00");
    // The plain (non-HTML) rule name. PHP's `testFilterDo` always sets
    // `$rule['inverse'] ??= false` before naming the rule, so the name
    // carries the `(inverse)` suffix even when inverse is off.
    assert_eq!(it["rules"].as_array().unwrap().len(), 1);
    assert_eq!(
        it["rules"][0].as_str().unwrap().trim(),
        "needle on Title in All feeds (inverse)"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_feed_scope(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Feed 1 (cat 1) and feed 4 (cat 2).
    add_entry(db, 101, "feed one article", "<p>x</p>", "", "", 1).await;
    add_entry(db, 102, "feed four article", "<p>x</p>", "", "", 4).await;

    // `feed_id: ["1"]` scopes the query to feed 1 only.
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "article", "filter_type": 1, "feed_id": ["1"]})]),
            ),
        ]),
    )
    .await;

    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "ue.feed_id = 1").await
    );
    assert!(
        items_for(&result, "feed one article").is_some(),
        "got {result}"
    );
    assert!(
        items_for(&result, "feed four article").is_none(),
        "got {result}"
    );
    assert_eq!(
        plain(
            items_for(&result, "feed one article").unwrap()[0]["rules"][0]
                .as_str()
                .unwrap()
        )
        .trim(),
        "article on Title in Hacker News (inverse)"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_category_scope(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "cat one feed two", "<p>x</p>", "", "", 2).await;
    add_entry(db, 102, "cat two feed four", "<p>x</p>", "", "", 4).await;

    // `feed_id: ["CAT:1"]` → feeds in category 1 (feeds 1–3), not feed 4.
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "feed", "filter_type": 1, "feed_id": ["CAT:1"]})]),
            ),
        ]),
    )
    .await;

    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "fc.id = 1").await
    );
    assert!(
        items_for(&result, "cat one feed two").is_some(),
        "got {result}"
    );
    assert!(
        items_for(&result, "cat two feed four").is_none(),
        "got {result}"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_uncategorized_scope(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // A feed without a category (the fixture feeds all have one).
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url) \
         VALUES (5, 1, 'Uncat Test', 'https://example.com/uncat.xml')",
    )
    .execute(db.pool())
    .await
    .expect("setup: uncat feed");
    add_entry(db, 101, "no category entry", "<p>x</p>", "", "", 5).await;
    add_entry(db, 102, "with category entry", "<p>x</p>", "", "", 1).await;

    // `feed_id: ["CAT:0"]` → `cat_id IS NULL` only.
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "entry", "filter_type": 1, "feed_id": ["CAT:0"]})]),
            ),
        ]),
    )
    .await;

    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "fc.id IS NULL").await
    );
    assert!(
        items_for(&result, "no category entry").is_some(),
        "got {result}"
    );
    assert!(
        items_for(&result, "with category entry").is_none(),
        "got {result}"
    );
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_content_preview(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(
        db,
        101,
        "Content article",
        "<p>prefix text and the secret word and suffix</p>",
        "",
        "",
        1,
    )
    .await;

    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "secret", "filter_type": 2, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;

    let it = &items_for(&result, "Content article").expect("item")[0];
    // The match is not at offset 0 → `&hellip;` prefix + the content from
    // the match, truncated to 200 chars, with the match highlighted (and
    // the DOM round-trip's outer `<span>`).
    let preview = it["content_preview"].as_str().unwrap();
    assert_eq!(
        plain(preview),
        "&hellip;secret word and suffix",
        "preview: {preview}"
    );
    assert!(
        preview.contains("<span class=\"highlight\">secret</span>"),
        "preview: {preview}"
    );
    // The title is not highlighted for a content rule (no DOM round-trip).
    assert_eq!(it["title"], "Content article");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_content_preview_strips_tags(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // The rule matches only inside a tag attribute (the href); the
    // tag-stripped preview text does not contain it.
    add_entry(
        db,
        101,
        "Linked article",
        "<p>notes at <a href=\"http://example.com/x\">the site</a></p>",
        "",
        "",
        1,
    )
    .await;

    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "http", "filter_type": 2, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;

    let it = &items_for(&result, "Linked article").expect("item")[0];
    // `mb_strpos` misses in the tag-stripped text → the preview starts at
    // the beginning (no `&hellip;` prefix), nothing is highlighted, and the
    // DOM round-trip still wraps the result in `<span>`.
    let preview = it["content_preview"].as_str().unwrap();
    assert_eq!(plain(preview), "notes at the site", "preview: {preview}");
    assert!(!preview.contains("highlight"), "preview: {preview}");
    assert!(preview.starts_with("<span>"), "preview: {preview}");
}
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_match_any_vs_all(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    // Title matches `aaa`; author does NOT match `bob`.
    add_entry(db, 101, "aaa title", "<p>x</p>", "alice", "", 1).await;

    let rules = rule_param(&[
        json!({"reg_exp": "aaa", "filter_type": 1, "feed_id": ["0"]}),
        json!({"reg_exp": "bob", "filter_type": 6, "feed_id": ["0"]}),
    ]);

    // match_any_rule: one rule matching is enough.
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            ("match_any_rule", json!("on")),
            ("rule", rules.clone()),
        ]),
    )
    .await;
    assert!(items_for(&result, "aaa title").is_some(), "got {result}");

    // match-all (default): both must match → no item.
    let result = do_test(
        &state,
        &sess,
        &request(&[("limit", json!("100")), ("rule", rules)]),
    )
    .await;
    assert!(items_for(&result, "aaa title").is_none(), "got {result}");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_inverse_all_match_empty_items(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "boring title", "<p>x</p>", "", "", 1).await;
    add_entry(db, 102, "exciting title", "<p>x</p>", "", "", 2).await;

    // `inverse` of a rule matching EVERY entry (`.*)` → everything excluded
    // → empty items (and no fatal — no matched entry builds an item).
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            ("inverse", json!("on")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": ".*", "filter_type": 1, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;
    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "true").await
    );
    assert_eq!(result["items"].as_array().unwrap().len(), 0);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_inverse_no_match_fatal(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "boring title", "<p>x</p>", "", "", 1).await;

    // `inverse` of a rule matching nothing → every entry "matches" with an
    // empty highlight word → the PHP max-execution-time fatal (the handler
    // reports the 500-empty-body case).
    let err = ttrss::filters::test_filter_do(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            ("inverse", json!("on")),
            (
                "rule",
                rule_param(&[
                    json!({"reg_exp": "zz_no_match_zz", "filter_type": 1, "feed_id": ["0"]}),
                ]),
            ),
        ]),
    )
    .await;
    assert!(err.is_err(), "expected the PHP-fatal Err");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_tag_rule(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "Tagged article", "<p>x</p>", "", "news,rust", 1).await;
    add_entry(db, 102, "Untagged article", "<p>x</p>", "", "", 2).await;

    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("limit", json!("100")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "rust", "filter_type": 7, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;

    let it = &items_for(&result, "Tagged article").expect("item")[0];
    // The tag preview: the material-icon span + the tag list (with the
    // matched tag highlighted; the DOM round-trip adds an outer `<span>`).
    let preview = it["content_preview"].as_str().unwrap();
    assert!(
        preview.contains("<i class=\"material-icons\">label_outline</i>"),
        "preview: {preview}"
    );
    assert!(plain(preview).contains("news, rust"), "preview: {preview}");
    assert!(
        preview.contains("<span class=\"highlight\">rust</span>"),
        "preview: {preview}"
    );
    assert!(items_for(&result, "Untagged article").is_none());
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_limit_offset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    for (id, i) in (0..5).enumerate() {
        add_entry(
            db,
            100 + id as i32,
            &format!("page entry {i}"),
            "<p>x</p>",
            "",
            "",
            1,
        )
        .await;
    }

    // `pre_filtering_count` is the page size (entries fetched); the test
    // entries sort above the seed entries (newer), so the page is filled
    // by matches.
    let result = do_test(
        &state,
        &sess,
        &request(&[
            ("offset", json!("2")),
            ("limit", json!("2")),
            (
                "rule",
                rule_param(&[json!({"reg_exp": "entry", "filter_type": 1, "feed_id": ["0"]})]),
            ),
        ]),
    )
    .await;
    assert_eq!(result["pre_filtering_count"], 2, "got {result}");
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_no_rules_no_match(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "Any article", "<p>x</p>", "", "", 1).await;

    // No `rule` params at all: the fake filter has no rules → nothing
    // matches (and no scope restriction).
    let result = do_test(&state, &sess, &request(&[("limit", json!("100"))])).await;
    assert_eq!(
        result["pre_filtering_count"],
        count_entries(db, "true").await
    );
    assert_eq!(result["items"].as_array().unwrap().len(), 0);
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_test_filter_do_invalid_rule_json_skipped(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = &state.db;

    add_entry(db, 101, "needle article", "<p>x</p>", "", "", 1).await;

    // The first `rule[]` value is malformed JSON (PHP's catch → continue);
    // the second is a valid matching rule.
    let mut rule_param =
        rule_param(&[json!({"reg_exp": "needle", "filter_type": 1, "feed_id": ["0"]})]);
    if let Some(arr) = rule_param.as_array_mut() {
        arr.insert(0, Value::String("not json".to_string()));
    }

    let result = do_test(
        &state,
        &sess,
        &request(&[("limit", json!("100")), ("rule", rule_param)]),
    )
    .await;
    assert!(
        items_for(&result, "needle article").is_some(),
        "got {result}"
    );
}

/// Regression for the PHP `$_REQUEST` `[]` normalization: the filter forms
/// post `rule[]`/`action[]` fields. Through the real `rpc::handler` (the SPA
/// request shape), `editSave` must save the rules — before the parse-boundary
/// fix the handler read the bare `rule` key and silently saved none.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql",
))]
async fn test_editsave_rule_bracket_fields(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::{header, Method};
    use axum::response::IntoResponse;

    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState::new(test_config(), db.clone());

    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);

    let id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, title, enabled) VALUES (1, 'Brk', true) RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .expect("insert filter");

    // A single `rule[]` field + a single `action[]` field (PHP: each is a
    // one-element array under the bare key).
    let body = format!(
        "op=Pref_Filters&method=editsave&csrf_token=tok&id={id}&title=Brk&rule[]=%7B%22reg_exp%22%3A%22brk%22%2C%22filter_type%22%3A1%2C%22feed_id%22%3A%5B%220%22%5D%7D&action[]=%7B%22action_id%22%3A2%2C%22action_param%22%3A%22%22%7D"
    );
    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/backend.php")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(body))
        .unwrap();
    let res = ttrss::rpc::handler(State(state.clone()), axum::extract::Extension(sess), req)
        .await
        .into_response();
    let bytes = to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    // `editsave` is a PHP void method → the body is empty (no JSON).
    assert!(bytes.is_empty(), "body: {bytes:?}");

    let rule_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_filters2_rules WHERE filter_id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("rule count");
    assert_eq!(rule_count, 1, "rule[] must be saved");

    let action_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_filters2_actions WHERE filter_id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("action count");
    assert_eq!(action_count, 1, "action[] must be saved");
}
