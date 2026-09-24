//! Test parity: M7.12 — the `cache_starred_images` plugin (scheduler-driven
//! media caching of starred articles).
//!
//! The PHP `cache_starred_images` plugin has no dedicated suite in the frozen
//! reference (the M7 "no per-plugin PHP suites" note); the cases here pin the
//! M7.12 verify bar: the task-run (scan the owner's starred, not-yet-cached
//! articles → download their media into the `starred-images` DiskCache → mark
//! them done in `plugin_data`) and the per-article attempt cap (5).

mod common;

use test_db::template::with_test_db;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::AppState;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique, writable base dir for the plugin's DiskCache instances, so
/// [`Config::base_cache_dir`] (the parent of `feed_icon_dir`) resolves to it.
fn cache_base() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ttrss_csi_{}_{}", std::process::id(), n));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

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
        auth_auto_create: true,
        auth_remote_post_logout_url: String::new(),
        plugins: "cache_starred_images".into(),
        listen: "127.0.0.1:8282".parse().unwrap(),
        docroot: PathBuf::from("tt-rss"),
        feed_icon_dir: PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        max_cache_file_size: 64 * 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_max_jobs: 2,
        daemon_unsuccessful_days_limit: 30,
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_expire_lock_files: String::from("@hourly"),
        fetch_extended_url_filtering: false,
        daemon_sleep_interval: 60,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@hourly"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
    }
}

async fn setup(url: &str, base: PathBuf) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let mut config = test_config();
    // `base_cache_dir()` is `feed_icon_dir.parent()` → the temp base.
    config.feed_icon_dir = base.join("feed-icons");
    AppState::new(config, db)
}

/// Invoke one of the plugin's registered scheduled tasks (by bare name) for
/// owner 1, exactly as the (deferred) scheduler would.
async fn run_task(state: &AppState, name: &str) {
    let task = state
        .plugins
        .scheduled_tasks()
        .iter()
        .find(|t| t.task_name == name)
        .unwrap_or_else(|| panic!("no scheduled task named {name:?}"));
    let cfg = state.config.clone();
    (*task.callback)(state.db.pool().clone(), Some(1), cfg).await;
}

/// The article's `plugin_data` (or `None`).
async fn plugin_data(pool: &sqlx::PgPool, article_id: i32) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT plugin_data FROM ttrss_entries WHERE id = $1")
        .bind(article_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .flatten()
}

/// Seed a starred article for owner 1 (feed with `site_url` + content + a
/// `marked` user_entry). Returns the article id.
async fn seed_starred_article(
    pool: &sqlx::PgPool,
    site_url: &str,
    content: &str,
) -> i32 {
    let feed_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_feeds (owner_uid, title, feed_url, site_url) \
         VALUES (1, 'Test feed', 'http://127.0.0.1:1/rss', $1) RETURNING id",
    )
    .bind(site_url)
    .fetch_one(pool)
    .await
    .unwrap();

    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let guid = format!("csi-guid-{}-{n}", std::process::id());
    let article_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_entries (title, guid, link, updated, content, content_hash, plugin_data, date_entered, date_updated) \
         VALUES ('Test article', $1, 'http://127.0.0.1:1/a1', now(), $2, 'csi-hash', '', now(), now()) RETURNING id",
    )
    .bind(&guid)
    .bind(content)
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, owner_uid, marked, tag_cache, label_cache, unread) \
         VALUES ($1, 'csi-1', $2, 1, true, '', '', true)",
    )
    .bind(article_id)
    .bind(feed_id)
    .execute(pool)
    .await
    .unwrap();

    article_id
}

#[tokio::test]
async fn test_task_caches_starred_media() {
    with_test_db(|url| Box::pin(async move {
    use common::{Canned, MockServer};

    let base = cache_base();
    let state = setup(&url, base.clone()).await;

    // A media file served over loopback.
    let body: Vec<u8> = b"fake-png-bytes-0123456789".to_vec();
    let server = MockServer::new(vec![Canned {
        status: 200,
        reason: "OK",
        headers: vec![("Content-Type", "image/png".to_string())],
        body: body.clone(),
    }]);
    let media_url = server.url("/img.png");
    let site_url = server.url("/");

    let article_id =
        seed_starred_article(state.db.pool(), &site_url, &format!("<p>hi</p><img src=\"{media_url}\">"))
            .await;

    run_task(&state, "cache_starred_images").await;

    // The article is marked done with the `starred_cache_images,<uid>` marker…
    let data = plugin_data(state.db.pool(), article_id).await;
    assert!(data
        .as_deref()
        .unwrap_or("")
        .contains("starred_cache_images,1"),
        "plugin_data = {data:?}");

    // …and its media is on disk in the `starred-images` cache (a `<id>-<sha1>`
    // file with the served bytes).
    let media_dir = base.join("starred-images");
    let cached = std::fs::read_dir(&media_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| e != "status")
                .unwrap_or(true)
                && !p.file_name().unwrap().to_string_lossy().starts_with('.')
        })
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{article_id}-")))
        .unwrap_or_else(|| panic!("no cached media file for {article_id} in {media_dir:?}"));
    assert_eq!(std::fs::read(&cached).unwrap(), body);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_attempt_cap() {
    with_test_db(|url| Box::pin(async move {
    let base = cache_base();
    let state = setup(&url, base.clone()).await;

    // An unreachable media URL (a closed loopback port → the fetch fails fast)
    // so `cache_article_images` keeps retrying, capped at 5 attempts.
    let article_id =
        seed_starred_article(state.db.pool(), "http://127.0.0.1:1/", "<img src=\"http://127.0.0.1:1/nope.png\">")
            .await;

    for _ in 0..6 {
        run_task(&state, "cache_starred_images").await;
    }

    // The per-article `.status` file records the capped attempt count (5)…
    let status_dir = base.join("starred-images.status-files");
    let status_file = std::fs::read_dir(&status_dir)
        .unwrap_or_else(|e| panic!("status dir {status_dir:?} missing: {e}"))
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|e| e == "status").unwrap_or(false))
        .unwrap_or_else(|| panic!("no .status file in {status_dir:?}"));
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(status_file).unwrap()).unwrap();
    assert_eq!(status["attempt"].as_i64(), Some(5));

    // …and the article is NOT marked done (the media never cached).
    assert_eq!(plugin_data(state.db.pool(), article_id).await, Some(String::new()));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_expire_removes_orphaned_media() {
    with_test_db(|url| Box::pin(async move {
    let base = cache_base();
    let state = setup(&url, base.clone()).await;

    // A cache file (and status file) for an article that does not exist.
    let media_dir = base.join("starred-images");
    let status_dir = base.join("starred-images.status-files");
    std::fs::create_dir_all(&media_dir).unwrap();
    std::fs::create_dir_all(&status_dir).unwrap();
    let orphan = media_dir.join("999-abc123");
    let orphan_status = status_dir.join("999-abc123.status");
    std::fs::write(&orphan, b"orphaned").unwrap();
    std::fs::write(&orphan_status, b"{}").unwrap();

    run_task(&state, "expire_caches").await;

    // Both orphaned files are gone (their article does not exist).
    assert!(!orphan.exists());
    assert!(!orphan_status.exists());
        Ok(())
    })).await.unwrap();
}
