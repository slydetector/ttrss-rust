//! The `cache_starred_images` plugin (`tt-rss/plugins/cache_starred_images/init.php`,
//! M7.12) — automatically caches media files in starred articles.
//!
//! **Scheduler-driven** (the plan's "queue on star" was a mischaracterization — the
//! reference registers no mark-toggled hook):
//! - `cache_starred_images` (`@hourly`): scans the owner's starred, not-yet-cached
//!   articles, downloads their media (content `<img>`/`<source>`/`<video>` + enclosures)
//!   into a `starred-images` [`DiskCache`], tracking per-article attempt counts (cap 5)
//!   in a `.status` file, then marks them cached in `plugin_data`.
//! - `expire_caches` (`@daily`): unlinks cached media (and status files) whose article
//!   is gone.
//! - `HOOK_SANITIZE` + `HOOK_ENCLOSURE_ENTRY` (read-side): rewrite media URLs to local
//!   cache paths when a cached copy exists.
//!
//! **Deliberate deviations from the PHP** — the port's `init` runs without a `Config`, so
//! the values PHP reads from `Config::get` at init time are pinned to their
//! `Config::_DEFAULTS`:
//! - The PHP gates the registrations on `$cache->is_writable()`; the port registers
//!   unconditionally and defers the writability check to the task bodies (which check
//!   `is_writable` before writing — safe either way).
//! - The PHP reads the schedules from `Config::get(SCHEDULE_*)` (overridable); the port
//!   pins them to the defaults (`@hourly` / `@daily`).
//! - The PHP puts `.no-auto-expiry` markers into both cache dirs at plugin init;
//!   the port's `init` has no `Config` (the substrate contract), so the markers
//!   are written at the start of the `cache_starred_images` task (before any
//!   download, so the housekeeping sweep can never see marker-less media).

use async_trait::async_trait;
use std::path::PathBuf;

use serde_json::Value;

use crate::core::config::Config;
use crate::diskcache::DiskCache;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::{HOOK_ENCLOSURE_ENTRY, HOOK_SANITIZE};
use crate::urlhelper::{self, FetchOptions};
use crate::util::sha1_hex;

use kuchikiki::traits::TendrilSink;

/// PHP's `add_scheduled_task` task names (the bare `$task_name` args).
const TASK_CACHE: &str = "cache_starred_images";
const TASK_EXPIRE: &str = "expire_caches";

/// PHP's `$max_cache_attempts` (per article).
const MAX_CACHE_ATTEMPTS: i64 = 5;

/// The `plugin_data` marker + owner prefix used to detect "already cached".
const MARKER_PREFIX: &str = "starred_cache_images,";

/// The `DiskCache::instance(...)` names.
const CACHE_NAME: &str = "starred-images";
const CACHE_STATUS_NAME: &str = "starred-images.status-files";

/// PHP's `class Cache_Starred_Images extends Plugin`.
#[derive(Clone, Copy)]
pub struct CacheStarredImages;

#[async_trait]
impl Plugin for CacheStarredImages {
    fn name(&self) -> &'static str {
        "cache_starred_images"
    }

    fn class_name(&self) -> &'static str {
        "Cache_Starred_Images"
    }

    fn about(&self) -> About {
        About::new("Automatically cache media files in Starred articles", "fox")
    }

    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        // The bodies are plain `fn` items (function items implement `Fn`), coerced to
        // the `Box<dyn Fn(PgPool, Option<i32>, Config) -> BoxFuture<'static, ()>>`.
        host.add_scheduled_task(TASK_CACHE, "@hourly", Box::new(run_cache_starred_images));
        host.add_scheduled_task(TASK_EXPIRE, "@daily", Box::new(run_expire_caches));
        host.add_hook_default(HOOK_ENCLOSURE_ENTRY);
        host.add_hook_default(HOOK_SANITIZE);
    }

    // -- HOOK_SANITIZE ------------------------------------------------------

    /// PHP's `hook_sanitize($doc, $site_url, …, $article_id)` — rewrite
    /// `<img>`/`<source>`/`<video>` `src` to the local cache URL (dropping `srcset`)
    /// when a cached copy exists. `doc` is the article HTML (a `&mut String` in the
    /// port; the PHP passes a `DOMDocument`).
    async fn hook_sanitize(
        &self,
        host: &mut HostCtx,
        doc: &mut String,
        site_url: &str,
        _allowed_elements: &[String],
        _disallowed_attributes: &[String],
        article_id: i32,
    ) -> () {
        if article_id == 0 {
            return;
        }
        let cache = DiskCache::new(&host.cache_dir, CACHE_NAME, &host.self_url);
        *doc = rewrite_sanitized_media(doc, site_url, article_id, &cache);
    }

    // -- HOOK_ENCLOSURE_ENTRY -----------------------------------------------

    /// PHP's `hook_enclosure_entry($entry, $article_id, $rv)` — rewrite the enclosure
    /// `content_url` to the local cache URL when a cached copy exists.
    async fn hook_enclosure_entry(
        &self,
        host: &mut HostCtx,
        entry: &mut Value,
        id: i32,
        _rv: &Value,
    ) -> Value {
        let mut out = entry.clone();
        if id != 0
            && let Some(url) = out.get("content_url").and_then(|v| v.as_str())
        {
            let cache = DiskCache::new(&host.cache_dir, CACHE_NAME, &host.self_url);
            let local = format!("{}-{}", id, sha1_hex(url));
            if cache.exists(&local) {
                out["content_url"] = Value::String(cache.get_url(&local));
            }
        }
        out
    }
}

// -- scheduled-task bodies ------------------------------------------------------

/// PHP's `cache_starred_images` scheduled-task closure — scan the owner's starred,
/// not-yet-cached articles and cache their media.
fn run_cache_starred_images(
    pool: sqlx::PgPool,
    owner_uid: Option<i32>,
    config: Config,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let Some(owner_uid) = owner_uid else { return };
        let cache_dir = config.cache_dir.clone();
        let cache = DiskCache::new(&cache_dir, CACHE_NAME, &config.self_url);
        let cache_status = DiskCache::new(&cache_dir, CACHE_STATUS_NAME, &config.self_url);

        // PHP `init`: exempt both dirs from the housekeeping sweep (the port
        // writes them here, before any download — see the module docs).
        if !cache.exists(crate::diskcache::NO_AUTO_EXPIRY) {
            cache.put(crate::diskcache::NO_AUTO_EXPIRY, b"");
        }
        if !cache_status.exists(crate::diskcache::NO_AUTO_EXPIRY) {
            cache_status.put(crate::diskcache::NO_AUTO_EXPIRY, b"");
        }

        // PHP's scan: starred, has a site_url, this owner, not yet marked cached.
        let rows: Vec<(String, String, i32, i32, Option<String>)> = sqlx::query_as(
            "SELECT e.content, f.site_url, ue.owner_uid, e.id, e.plugin_data \
             FROM ttrss_entries e, ttrss_user_entries ue \
             LEFT JOIN ttrss_feeds f ON (ue.feed_id = f.id) \
             WHERE ue.ref_id = e.id AND ue.marked = true AND f.site_url != '' \
               AND ue.owner_uid = $1 AND e.plugin_data NOT LIKE '%starred_cache_images%' \
             ORDER BY RANDOM() LIMIT 100",
        )
        .bind(owner_uid)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

        for (content, site_url, _owner, id, plugin_data) in rows {
            if site_url.is_empty() {
                continue;
            }
            if cache_article_images(
                &pool,
                &cache,
                &cache_status,
                &content,
                &site_url,
                id,
                &config,
            )
            .await
            {
                let new_data = format!(
                    "{}{}:{}",
                    MARKER_PREFIX,
                    _owner,
                    plugin_data.unwrap_or_default()
                );
                let _ = sqlx::query("UPDATE ttrss_entries SET plugin_data = $1 WHERE id = $2")
                    .bind(&new_data)
                    .bind(id)
                    .execute(&pool)
                    .await;
            }
        }
    })
}

/// PHP's `expire_caches` scheduled-task closure — unlink cached media (and status files)
/// whose article is gone.
fn run_expire_caches(
    pool: sqlx::PgPool,
    _owner_uid: Option<i32>,
    config: Config,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let cache_dir = config.cache_dir.clone();
        let cache = DiskCache::new(&cache_dir, CACHE_NAME, &config.self_url);
        let cache_status = DiskCache::new(&cache_dir, CACHE_STATUS_NAME, &config.self_url);

        // PHP: `glob($cache->get_dir() . "/*-*")` + `glob($cache_status->get_dir() .
        // "*.status")` — both are `<article_id>-<hash>…`, so collect every file with a
        // numeric `<article_id>` prefix (the `.no-auto-expiry` marker has none).
        let mut files: Vec<(i64, PathBuf)> = Vec::new();
        for dir in [cache.get_dir(), cache_status.get_dir()] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Some((aid, _)) = name.split_once('-')
                        && let Ok(aid) = aid.parse::<i64>()
                    {
                        files.push((aid, entry.path()));
                    }
                }
            }
        }
        // Group by article_id so the existence check runs once per article (PHP's
        // `asort` groups by full path — a coarser, equivalent grouping for correctness).
        files.sort_by_key(|(aid, path)| (*aid, path.clone()));

        let mut last_article_id = 0i64;
        let mut article_exists = true;
        for (article_id, path) in files {
            if article_id != last_article_id {
                last_article_id = article_id;
                article_exists =
                    sqlx::query_scalar::<_, i32>("SELECT id FROM ttrss_entries WHERE id = $1")
                        .bind(article_id as i32)
                        .fetch_optional(&pool)
                        .await
                        .is_ok_and(|r| r.is_some());
            }
            if !article_exists {
                let _ = std::fs::remove_file(&path);
            }
        }
    })
}

// -- per-article cache + helpers ------------------------------------------------

/// PHP's `cache_article_images($content, $site_url, $owner_uid, $article_id)` — the
/// per-article download: track the attempt count (cap [`MAX_CACHE_ATTEMPTS`]) in a
/// `.status` file, then download the content's media + the enclosures. Returns `true` if
/// any media cached **or** no media was found (so the article is marked done), else
/// `false` (retry on a later run, up to the cap).
async fn cache_article_images(
    pool: &sqlx::PgPool,
    cache: &DiskCache,
    cache_status: &DiskCache,
    content: &str,
    site_url: &str,
    article_id: i32,
    config: &Config,
) -> bool {
    let status_filename = format!("{}-{}.status", article_id, sha1_hex(site_url));

    // PHP: `if (!$this->cache_status->is_writable($status_filename)) return false;`
    if !cache_status.is_writable(Some(&status_filename)) {
        return false;
    }

    // Load the attempt count from the status file (default 0).
    let mut attempt: i64 = 0;
    if cache_status.exists(&status_filename)
        && let Some(s) = cache_status.get(&status_filename)
    {
        attempt = serde_json::from_str::<Value>(&s)
            .ok()
            .and_then(|v| v.get("attempt").and_then(|a| a.as_i64()))
            .unwrap_or(0);
    }
    attempt += 1;

    // PHP: `if ($status["attempt"] > $this->max_cache_attempts) return false;`
    if attempt > MAX_CACHE_ATTEMPTS {
        return false;
    }

    // PHP: `if (!$this->cache_status->put($status_filename, json_encode($status))) …`.
    let status = serde_json::json!({ "attempt": attempt });
    if !cache_status.put(&status_filename, status.to_string().as_bytes()) {
        return false;
    }

    // Cache the content's media.
    let mut has_media = false;
    let mut success = false;
    for url in crate::rss_utils::media_urls(content, site_url) {
        has_media = true;
        if cache_url(
            cache,
            pool,
            article_id,
            &url,
            config.max_cache_file_size as u64,
        )
        .await
        {
            success = true;
        }
    }

    // PHP's enclosure pass: the article's image/video enclosures.
    let encs: Vec<String> = sqlx::query_scalar(
        "SELECT content_url FROM ttrss_enclosures WHERE post_id = $1 \
         AND (content_type LIKE '%image%' OR content_type LIKE '%video%')",
    )
    .bind(article_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for url in encs {
        has_media = true;
        if let Some(rewritten) = urlhelper::rewrite_relative(site_url, &url)
            && cache_url(
                cache,
                pool,
                article_id,
                &rewritten,
                config.max_cache_file_size as u64,
            )
            .await
        {
            success = true;
        }
    }

    // PHP: `return $success || !$has_media;`
    success || !has_media
}

/// PHP's `cache_url($article_id, $url)` — download `$url` to the cache as
/// `<article_id>-<sha1($url)>` (a hit is a no-op). Returns whether the file is (now)
/// cached.
async fn cache_url(
    cache: &DiskCache,
    _pool: &sqlx::PgPool,
    article_id: i32,
    url: &str,
    max_size: u64,
) -> bool {
    let local = format!("{}-{}", article_id, sha1_hex(url));
    if cache.exists(&local) {
        return true;
    }
    // PHP: `UrlHelper::fetch(["url" => $url, "max_size" => MAX_CACHE_FILE_SIZE])`.
    let result = urlhelper::fetch(&FetchOptions {
        url: url.to_string(),
        max_size: Some(max_size),
        filtering: Some(true),
        ..Default::default()
    })
    .await;
    match result.body {
        Some(bytes) if !bytes.is_empty() => cache.put(&local, &bytes),
        _ => false,
    }
}

/// The `hook_sanitize` body — rewrite `<img>`/`<source>`/`<video>` `src` to the local
/// cache URL (dropping `srcset`) when a cached copy exists. Re-serializes the `<body>`
/// contents (the `diskcache::rewrite_urls` pattern).
fn rewrite_sanitized_media(
    html: &str,
    site_url: &str,
    article_id: i32,
    cache: &DiskCache,
) -> String {
    let doc = kuchikiki::parse_html().one(html);
    let mut changed = false;
    for node in doc.descendants() {
        let Some(el) = node.as_element() else {
            continue;
        };
        let name = el.name.local.to_string();
        if !matches!(name.as_str(), "img" | "source" | "video") {
            continue;
        }
        let Some(src) = el.attributes.borrow().get("src").map(str::to_string) else {
            continue;
        };
        if let Some(abs) = urlhelper::rewrite_relative(site_url, &src) {
            let local = format!("{}-{}", article_id, sha1_hex(&abs));
            if cache.exists(&local) {
                el.attributes
                    .borrow_mut()
                    .insert("src", cache.get_url(&local));
                el.attributes.borrow_mut().remove("srcset");
                changed = true;
            }
        }
    }
    if !changed {
        return html.to_string();
    }
    let mut out = String::new();
    if let Some(body) = crate::sanitizer::find_body(&doc) {
        for child in body.children() {
            crate::sanitizer::serialize_node(&child, &mut out);
        }
    }
    out
}
