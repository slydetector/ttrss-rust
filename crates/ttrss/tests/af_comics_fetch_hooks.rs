//! M7.22 item 8 — the af_comics fetch-time plugin hooks (`HOOK_FETCH_FEED`,
//! `HOOK_SUBSCRIBE_FEED`, `HOOK_FEED_BASIC_INFO`) are dispatched by the core.
//!
//! None of the shipped af_comics filters override `on_fetch` / `on_subscribe` /
//! `on_basic_info` (all return `false`), so for the stock set these dispatch
//! sites are no-ops — exactly as in PHP. These tests prove the *dispatch*
//! itself: a registered plugin's hook is invoked at the right point and its
//! return value is used (substituting the feed data / content / basic info),
//! decoupled from af_comics's per-comic filter logic via a stub plugin per hook.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::feeds::subscribe;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::{
    About, Plugin, PluginHost, HOOK_FETCH_FEED, HOOK_FEED_BASIC_INFO, HOOK_SUBSCRIBE_FEED,
};
use ttrss::rss_utils::update_basic_info;
use ttrss::updates::{fetch_one_feed, FeedUpdateFlags};
use common::{Canned, MockServer};
use test_db::template::with_test_db;

/// A valid RSS feed with a single article (the `HOOK_FETCH_FEED` stub's output).
const STUB_FETCH_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>STUB FETCH FEED</title>
    <link>http://stub.example.com</link>
    <item>
      <title>Stub Fetch Article</title>
      <link>http://stub.example.com/article-1</link>
      <guid>stub-fetch-1</guid>
    </item>
  </channel>
</rss>"#;

/// A valid RSS feed (the `HOOK_SUBSCRIBE_FEED` stub's output).
const STUB_SUBSCRIBE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>STUB SUBSCRIBE FEED</title>
    <link>http://stub.example.com</link>
    <item>
      <title>Stub Subscribe Article</title>
      <link>http://stub.example.com/article-1</link>
      <guid>stub-subscribe-1</guid>
    </item>
  </channel>
</rss>"#;

// ---------------------------------------------------------------------------
// Stub plugins — each registers exactly one fetch-time hook so the three
// tests isolate a single dispatch site.
// ---------------------------------------------------------------------------

/// Registers only `HOOK_FETCH_FEED`; returns the canned feed data.
struct FetchStub;

#[async_trait]
impl Plugin for FetchStub {
    fn name(&self) -> &'static str {
        "fetch_stub"
    }
    fn class_name(&self) -> &'static str {
        "Fetch_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_FETCH_FEED);
    }
    async fn hook_fetch_feed(
        &self,
        _host: &mut HostCtx,
        _feed_data: &str,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed: i32,
        _last_article_timestamp: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        STUB_FETCH_XML.to_string()
    }
}

/// Registers only `HOOK_SUBSCRIBE_FEED`; returns the canned content.
struct SubscribeStub;

#[async_trait]
impl Plugin for SubscribeStub {
    fn name(&self) -> &'static str {
        "subscribe_stub"
    }
    fn class_name(&self) -> &'static str {
        "Subscribe_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_SUBSCRIBE_FEED);
    }
    async fn hook_subscribe_feed(
        &self,
        _host: &mut HostCtx,
        _contents: &str,
        _url: &str,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        STUB_SUBSCRIBE_XML.to_string()
    }
}

/// Registers only `HOOK_FEED_BASIC_INFO`; returns canned title/site_url.
struct BasicInfoStub;

#[async_trait]
impl Plugin for BasicInfoStub {
    fn name(&self) -> &'static str {
        "basic_info_stub"
    }
    fn class_name(&self) -> &'static str {
        "Basic_Info_Stub"
    }
    fn about(&self) -> About {
        About::new("stub (test)", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_FEED_BASIC_INFO);
    }
    async fn hook_feed_basic_info(
        &self,
        _host: &mut HostCtx,
        _basic_info: &mut Value,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed_id: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> Value {
        json!({ "title": "STUB BASIC TITLE", "site_url": "http://basic.stub.example" })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> Config {
    Config {
        db_host: String::new(),
        db_port: 0,
        db_user: String::new(),
        db_name: String::new(),
        db_sslmode: String::new(),
        self_url: String::new(),
        plugins: String::new(),
        listen: "127.0.0.1:0".parse().unwrap(),
        docroot: std::path::PathBuf::new(),
        cache_dir: std::path::PathBuf::new(),
        max_favicon_file_size: 0,
        feed_fetch_timeout: 5,
        feed_fetch_no_cache_timeout: 3,
        http_429_throttle_interval: 3600,
        daemon_sleep_interval: 60,
        ..Default::default()
    }
}

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

async fn create_feed(db: &Db, id: i32, owner_uid: i32, title: &str, feed_url: &str) -> i32 {
    sqlx::query("INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(owner_uid)
        .bind(title)
        .bind(feed_url)
        .execute(db.pool())
        .await
        .expect("create feed");
    id
}

async fn feed_title(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("feed title")
}

async fn feed_site_url(db: &Db, feed_id: i32) -> String {
    sqlx::query_scalar("SELECT site_url FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_one(db.pool())
        .await
        .expect("feed site_url")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// `update_basic_info` runs `HOOK_FEED_BASIC_INFO` first: a plugin supplying a
// non-empty title/site_url short-circuits the chain and the fetch is skipped
// entirely (the feed URL below points at nothing, so only the hook could
// populate the fields).
#[tokio::test]
async fn test_feed_basic_info_hook_supplies_title_skipping_fetch() {
    with_test_db(|url| Box::pin(async move {
        let db = common::make_db_from_url(&url).await.expect("test db");
        let mut cfg = test_config();
        cfg.plugins = "basic_info_stub".to_string();
        let uid = create_user(&db, 400, 0).await;
        let feed_id = create_feed(
            &db,
            401,
            uid,
            "[Unknown]",
            "http://nonexistent.invalid/feed.xml",
        )
        .await;

        let host = Arc::new(PluginHost::startup(&[|| Box::new(BasicInfoStub)]));
        update_basic_info(db.pool(), &cfg, &host, feed_id as i32).await;

        assert_eq!(feed_title(&db, feed_id as i32).await, "STUB BASIC TITLE");
        assert_eq!(
            feed_site_url(&db, feed_id as i32).await,
            "http://basic.stub.example"
        );
        Ok(())
    }))
    .await
    .unwrap();
}

// `fetch_one_feed` runs `HOOK_FETCH_FEED` before fetching: a plugin supplying
// non-empty feed data skips the fetch and the data is parsed directly (the feed
// URL below points at nothing, so only the hook's data could be ingested).
#[tokio::test]
async fn test_fetch_feed_hook_supplies_data_skipping_fetch() {
    with_test_db(|url| Box::pin(async move {
        let db = common::make_db_from_url(&url).await.expect("test db");
        let mut cfg = test_config();
        cfg.plugins = "fetch_stub".to_string();
        let uid = create_user(&db, 410, 0).await;
        let feed_id = create_feed(
            &db,
            411,
            uid,
            "Test Feed",
            "http://nonexistent.invalid/feed.xml",
        )
        .await;

        let host = Arc::new(PluginHost::startup(&[|| Box::new(FetchStub)]));
        let outcome = fetch_one_feed(
            db.pool(),
            &cfg,
            Arc::clone(&host),
            feed_id as i32,
            &FeedUpdateFlags::default(),
            None,
        )
        .await
        .expect("fetch");

        assert!(
            outcome.success,
            "the hook-supplied feed should parse, got {:?}",
            outcome.error
        );
        assert_eq!(outcome.new_articles, 1, "the hook's single article should be ingested");
        Ok(())
    }))
    .await
    .unwrap();
}

// `subscribe` runs `HOOK_SUBSCRIBE_FEED` after the fetch, before the empty /
// parse checks: the mock serves content that is *not* a feed (non-HTML, invalid
// as a feed), so without the hook subscribe would fail to parse (code 6); the
// hook replacing it with a valid feed makes the feed be created (code 1).
#[tokio::test]
async fn test_subscribe_feed_hook_transforms_content() {
    with_test_db(|url| Box::pin(async move {
        let db = common::make_db_from_url(&url).await.expect("test db");
        let mut cfg = test_config();
        cfg.plugins = "subscribe_stub".to_string();
        let uid = create_user(&db, 420, 0).await;
        let mock = MockServer::new(vec![Canned {
            status: 200,
            reason: "OK",
            headers: vec![("content-type", "text/plain".to_string())],
            body: b"this is not a feed".to_vec(),
        }]);

        let host = Arc::new(PluginHost::startup(&[|| Box::new(SubscribeStub)]));
        let rc = subscribe(
            &cfg,
            db.pool(),
            &host,
            uid as i64,
            0,
            &mock.url("/feed"),
            0,
            "",
            "",
            0,
        )
        .await;

        assert_eq!(
            rc.get("code").and_then(|v| v.as_i64()),
            Some(1),
            "the hook's valid content should be parsed and the feed created: {rc}"
        );
        Ok(())
    }))
    .await
    .unwrap();
}
