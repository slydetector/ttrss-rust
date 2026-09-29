//! The feed-pipeline plugin hooks (G1 wiring): `HOOK_FEED_FETCHED`,
//! `HOOK_FEED_PARSED`, `HOOK_ENCLOSURE_IMPORTED`, `HOOK_FILTER_TRIGGERED`,
//! `HOOK_ARTICLES_MARK_TOGGLED`, `HOOK_ARTICLES_PUBLISH_TOGGLED`,
//! `HOOK_HOUSE_KEEPING`.
//!
//! None of the 18 first-party plugins implements any of these, so the
//! dispatch sites are no-ops for the stock set (exactly as in PHP). These
//! tests prove the *dispatch* itself: a registered stub plugin's hook is
//! invoked at the right point, and its rewrite flows through the pipeline.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use ttrss::core::db::Db;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::{
    About, Plugin, PluginHost, HOOK_ARTICLES_MARK_TOGGLED, HOOK_ARTICLES_PUBLISH_TOGGLED,
    HOOK_ENCLOSURE_IMPORTED, HOOK_FEED_FETCHED, HOOK_FEED_PARSED, HOOK_FILTER_TRIGGERED,
    HOOK_HOUSE_KEEPING,
};
use ttrss::updates::{fetch_one_feed, run_house_keeping_hook, FeedRow, FeedUpdateFlags};
use test_db::template::with_test_db;

/// A feed with one article (guid `pipe-1`) and one enclosure.
const PIPE_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>PIPE STUB FEED</title>
    <link>http://pipe.example.com</link>
    <item>
      <title>Pipe Article</title>
      <link>http://pipe.example.com/article-1</link>
      <guid>pipe-1</guid>
      <pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate>
      <enclosure url="http://pipe.example.com/media.mp3" type="audio/mpeg" length="1234"/>
    </item>
  </channel>
</rss>"#;

/// One stub for all seven pipeline hooks: records each call and rewrites the
/// feed XML's title / the enclosure's link (the result-threading proof).
struct PipeStub {
    calls: Arc<Mutex<Vec<String>>>,
}

impl PipeStub {
    fn record(&self, what: impl Into<String>) {
        self.calls.lock().unwrap().push(what.into());
    }
}

#[async_trait]
impl Plugin for PipeStub {
    fn name(&self) -> &'static str {
        "pipe_stub"
    }
    fn class_name(&self) -> &'static str {
        "Pipe_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_FEED_FETCHED);
        host.add_hook_default(HOOK_FEED_PARSED);
        host.add_hook_default(HOOK_ENCLOSURE_IMPORTED);
        host.add_hook_default(HOOK_FILTER_TRIGGERED);
        host.add_hook_default(HOOK_ARTICLES_MARK_TOGGLED);
        host.add_hook_default(HOOK_ARTICLES_PUBLISH_TOGGLED);
        host.add_hook_default(HOOK_HOUSE_KEEPING);
    }

    async fn hook_feed_fetched(
        &self,
        _host: &mut HostCtx,
        feed_data: &str,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed: i32,
    ) -> String {
        self.record("feed_fetched");
        feed_data.replace("<title>Pipe Article</title>", "<title>HOOKED Article</title>")
    }

    async fn hook_feed_parsed(&self, _host: &mut HostCtx, _parser: &ttrss::core::feed::FeedParser<'_>, feed_id: i32) {
        self.record(format!("feed_parsed:{feed_id}"));
    }

    async fn hook_enclosure_imported(
        &self,
        _host: &mut HostCtx,
        enclosure: &mut Value,
        _feed: i32,
    ) -> Value {
        self.record("enclosure_imported");
        if let Some(link) = enclosure.get("link").and_then(Value::as_str) {
            enclosure["link"] = Value::String(format!("{link}?hooked=1"));
        }
        enclosure.clone()
    }

    async fn hook_filter_triggered(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _owner_uid: i32,
        _article: &Value,
        matched_filters: &Value,
        _matched_rules: &Value,
        _actions: &Value,
    ) {
        let n = matched_filters.as_array().map_or(0, |a| a.len());
        self.record(format!("filter_triggered:matched={n}"));
    }

    async fn hook_articles_mark_toggled(&self, _host: &mut HostCtx, article_ids: &[i32]) {
        self.record(format!("articles_mark_toggled:{article_ids:?}"));
    }

    async fn hook_articles_publish_toggled(&self, _host: &mut HostCtx, article_ids: &[i32]) {
        self.record(format!("articles_publish_toggled:{article_ids:?}"));
    }

    async fn hook_house_keeping(&self, _host: &mut HostCtx) {
        self.record("house_keeping");
    }
}

fn setup_host(calls: Arc<Mutex<Vec<String>>>) -> Arc<PluginHost> {
    let mut host = PluginHost::new();
    host.register(Box::new(PipeStub { calls }));
    Arc::new(host)
}

fn pipe_feed_row() -> FeedRow {
    FeedRow {
        id: 1,
        owner_uid: 1,
        title: "Hacker News".into(),
        feed_url: "http://pipe.example.com/rss".into(),
        site_url: "http://pipe.example.com".into(),
        feed_language: String::new(),
        mark_unread_on_update: false,
        cache_images: false,
        purge_interval: 0,
        last_unconditional: None,
        last_modified: String::new(),
        auth_login: String::new(),
        auth_pass: String::new(),
        favicon_is_custom: false,
        favicon_avg_color: None,
        favicon_needs_check: false,
    }
}

/// Insert an enabled filter for owner 1 matching `title_re`, with the given
/// `action_id` (3 = mark, 5 = publish).
async fn insert_match_filter(pool: &sqlx::PgPool, title_re: &str, action_id: i32) -> i32 {
    let id: i32 =
        sqlx::query_scalar("INSERT INTO ttrss_filters2 (owner_uid, title, enabled) VALUES (1, 'pipe-test', true) RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type) VALUES ($1, $2, 1)")
        .bind(id)
        .bind(title_re)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, $2, '')")
        .bind(id)
        .bind(action_id)
        .execute(pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn test_feed_fetched_and_parsed_dispatched() {
    with_test_db(|url| Box::pin(async move {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let pool = db.pool();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = setup_host(calls.clone());

    // Point the seed's feed 1 at the mock server.
    let server = common::MockServer::new(vec![common::Canned {
        status: 200,
        reason: "OK",
        headers: vec![("Content-Type", "application/rss+xml; charset=utf-8".to_string())],
        body: PIPE_FEED.as_bytes().to_vec(),
    }]);
    sqlx::query("UPDATE ttrss_feeds SET feed_url = $1 WHERE id = 1")
        .bind(server.url("/feed.xml"))
        .execute(pool)
        .await
        .unwrap();

    let mut cfg = common::test_config();
    cfg.plugins = "pipe_stub".to_string();
    let outcome = fetch_one_feed(pool, &cfg, host, 1, &FeedUpdateFlags::default(), None).await.unwrap();
    assert!(outcome.success, "fetch ok: {:?}", outcome.error);
    assert_eq!(outcome.new_articles, 1);

    // The stub's title rewrite flowed into the parsed article.
    let title: String = sqlx::query_scalar("SELECT title FROM ttrss_entries WHERE title = 'HOOKED Article'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(title, "HOOKED Article");

    let calls = calls.lock().unwrap().join("\n");
    assert!(calls.contains("feed_fetched"), "calls: {calls}");
    assert!(calls.contains("feed_parsed:1"), "calls: {calls}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_enclosure_imported_rewrites() {
    with_test_db(|url| Box::pin(async move {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let pool = db.pool();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = setup_host(calls.clone());
    let mut cfg = common::test_config();
    cfg.plugins = "pipe_stub".to_string();

    let result = ttrss::updates::process_feed(
        pool,
        &cfg,
        host,
        &pipe_feed_row(),
        PIPE_FEED,
        &FeedUpdateFlags::default(),
        None,
    )
    .await
    .unwrap();
    assert!(result.parse_error.is_none(), "{:?}", result.parse_error);
    assert_eq!(result.new_articles, 1);

    // The hook's link rewrite survived into the enclosure table.
    let content_url: String =
        sqlx::query_scalar("SELECT content_url FROM ttrss_enclosures WHERE post_id = (SELECT id FROM ttrss_entries WHERE title = 'Pipe Article')")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(content_url.contains("?hooked=1"), "{content_url}");

    let calls = calls.lock().unwrap().join("\n");
    assert!(calls.contains("enclosure_imported"), "calls: {calls}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_filter_triggered_and_mark_toggled() {
    with_test_db(|url| Box::pin(async move {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let pool = db.pool();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = setup_host(calls.clone());
    let mut cfg = common::test_config();
    cfg.plugins = "pipe_stub".to_string();

    // A filter matching the article title, with the `mark` action.
    insert_match_filter(pool, "Pipe Article", 3).await;

    let result = ttrss::updates::process_feed(
        pool, &cfg, host, &pipe_feed_row(), PIPE_FEED, &FeedUpdateFlags::default(), None,
    )
    .await
    .unwrap();
    assert_eq!(result.new_articles, 1);

    let ref_id: i32 =
        sqlx::query_scalar("SELECT id FROM ttrss_entries WHERE title = 'Pipe Article'")
            .fetch_one(pool)
            .await
            .unwrap();
    let marked: bool = sqlx::query_scalar(
        "SELECT marked FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = 1",
    )
    .bind(ref_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(marked, "the mark action created a marked entry");

    let calls = calls.lock().unwrap().join("\n");
    assert!(
        calls.contains("filter_triggered:matched=1"),
        "calls: {calls}"
    );
    assert!(
        calls.contains(&format!("articles_mark_toggled:[{ref_id}]")),
        "calls: {calls}"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_publish_toggled() {
    with_test_db(|url| Box::pin(async move {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let pool = db.pool();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = setup_host(calls.clone());
    let mut cfg = common::test_config();
    cfg.plugins = "pipe_stub".to_string();

    insert_match_filter(pool, "Pipe Article", 5).await; // publish

    let result = ttrss::updates::process_feed(
        pool, &cfg, host, &pipe_feed_row(), PIPE_FEED, &FeedUpdateFlags::default(), None,
    )
    .await
    .unwrap();
    assert_eq!(result.new_articles, 1);

    let ref_id: i32 =
        sqlx::query_scalar("SELECT id FROM ttrss_entries WHERE title = 'Pipe Article'")
            .fetch_one(pool)
            .await
            .unwrap();
    let published: bool = sqlx::query_scalar(
        "SELECT published FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = 1",
    )
    .bind(ref_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(published);

    let calls = calls.lock().unwrap().join("\n");
    assert!(
        calls.contains(&format!("articles_publish_toggled:[{ref_id}]")),
        "calls: {calls}"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_house_keeping_dispatched() {
    with_test_db(|url| Box::pin(async move {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let pool = db.pool();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = setup_host(calls.clone());
    let mut cfg = common::test_config();
    cfg.plugins = "pipe_stub".to_string();

    run_house_keeping_hook(pool, &cfg, &host).await;

    assert_eq!(calls.lock().unwrap().join("\n"), "house_keeping");
        Ok(())
    })).await.unwrap();
}
