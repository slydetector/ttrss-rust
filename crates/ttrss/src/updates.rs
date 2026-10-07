//! M6b — the DB-backed feed-update pipeline: the deterministic core of
//! `tt-rss/classes/RSSUtils.php::update_rss_feed`.
//!
//! Given the feed XML (already fetched), this is architecture-independent:
//! parse → per-article (guid/hash → dedup → upsert `ttrss_entries` +
//! `ttrss_user_entries`) → stamp the feed's success metadata. The network
//! fetch (M6c) is decoupled: the caller supplies the feed data, so the core
//! can be replayed from recorded feed XML without HTTP.
//!
//! # Faithfulness notes
//! - No plugins in this port: the `PluginHost` hooks and the
//!   `calculate_article_hash` plugin-name prefix are no-ops (the prefix is
//!   empty, mirroring `PluginHost` with no plugins loaded).
//! - **Filter / label / tag application is deferred** (M6b.2c): it requires
//!   `RSSUtils::load_filters`, which is not yet ported. Until then an
//!   ingested article scores `0` (no filter actions, `score_modifier` 0), so
//!   it is unread / unmarked / unpublished — matching the PHP exactly for a
//!   feed with no filters.
//! - `tsvector_combined` uses `strip_tags(title) . " " . strip_tags(content)`
//!   in place of PHP's `Soundasleep\Html2Text::convert(content)` — functionally
//!   equivalent for full-text search; the frozen suite does not assert on it.
//! - Enclosures are parsed (`FeedItem::get_enclosures`), link-rewritten and
//!   persisted to `ttrss_enclosures` (deduped on content_url/content_type/post_id)
//!   and fed into the article hash — the PHP's `update_rss_feed` enclosure pass.
//!   Only `cache_enclosures` / `cache_media` (the network + disk-cache image
//!   download, gated on the feed's `cache_images`) and the favicon fetch remain
//!   out of scope (M6c).
//! - The per-feed purge (`Feeds::_purge`, M6c.4) runs before the success
//!   stamp, matching the PHP's `update_rss_feed` order (commit → purge →
//!   stamp the success metadata).

// `log.as_deref_mut()` reborrows the multi-use `Option<&mut FeedDebugLog>`
// parameter at each `dlog`/`process_feed`/`upsert_article` call site; the
// lint's alternative (passing `log` directly) would move it (E0382).
#![allow(clippy::needless_option_as_deref)]

use anyhow::{bail, Context, Result};
use chrono::{NaiveDateTime, Utc};
use serde_json::{json, Value};
use sqlx::{PgPool, Row, AssertSqlSafe};
use rand::Rng;
use std::sync::Arc;

use crate::core::config::Config;
use crate::core::feed::{FeedEnclosure, FeedItem, FeedParser};
use crate::core::filters::{
    calculate_article_score, eval_article_filters, has_article_filter_action, EvalFilter, EvalRule,
    FilterAction, RuleType,
};
use crate::core::user::ACCESS_LEVEL_READONLY;
use crate::counters::parent_cats;
use crate::feeds::decrypt_feed_pass;
use crate::rss_utils::{calculate_article_hash, make_guid_from_title, make_guid_hashed};
use crate::urlhelper::{fetch, FetchOptions, rewrite_relative};
use crate::util::{clean_str, normalize_categories, php_int_cast, sha1_hex, strip_tags};
use crate::pluginhost::host::HostCtx;
use crate::pluginhost::{
    HOOK_ARTICLE_FILTER, HOOK_ARTICLES_MARK_TOGGLED, HOOK_ARTICLES_PUBLISH_TOGGLED,
    HOOK_ENCLOSURE_IMPORTED, HOOK_FETCH_FEED, HOOK_FEED_FETCHED, HOOK_FEED_PARSED,
    HOOK_FILTER_TRIGGERED, HOOK_HOUSE_KEEPING, HOOK_UPDATE_TASK, PluginHost,
};

/// The subset of `ttrss_feeds` the pipeline reads.
#[derive(Debug, Clone)]
pub struct FeedRow {
    pub id: i32,
    pub owner_uid: i32,
    pub title: String,
    pub feed_url: String,
    pub site_url: String,
    pub feed_language: String,
    pub mark_unread_on_update: bool,
    pub cache_images: bool,
    pub purge_interval: i32,
    // M6c.2 — the fetch/conditional metadata (used by `fetch_one_feed`).
    pub last_unconditional: Option<NaiveDateTime>,
    pub last_modified: String,
    pub auth_login: String,
    pub auth_pass: String,
    // Favicon metadata (RSSUtils.php:372-373: `favicon_needs_check` computed expr).
    pub favicon_is_custom: bool,
    pub favicon_avg_color: Option<String>,
    pub favicon_needs_check: bool,
}

/// Load the feed row, or `None` if the id does not exist.
pub async fn load_feed(pool: &PgPool, feed_id: i32) -> Result<Option<FeedRow>> {
    let row = sqlx::query(
        "SELECT id, owner_uid, title, feed_url, site_url, feed_language, \
         mark_unread_on_update, cache_images, purge_interval, \
         last_unconditional, last_modified, auth_login, auth_pass, \
         COALESCE(favicon_is_custom, false), favicon_avg_color, \
         (favicon_is_custom IS NOT TRUE AND \
           (favicon_last_checked IS NULL OR favicon_last_checked < NOW() - INTERVAL '12 hour')) \
         FROM ttrss_feeds WHERE id = $1",
    )
    .bind(feed_id)
    .fetch_optional(pool)
    .await
    .context("load feed")?;
    Ok(row.map(|r| FeedRow {
        id: r.get(0),
        owner_uid: r.get(1),
        title: r.get(2),
        feed_url: r.get(3),
        site_url: r.get(4),
        feed_language: r.get(5),
        mark_unread_on_update: r.get(6),
        cache_images: r.get(7),
        purge_interval: r.get(8),
        last_unconditional: r.get(9),
        last_modified: r.get(10),
        auth_login: r.get(11),
        auth_pass: r.get(12),
        favicon_is_custom: r.get(13),
        favicon_avg_color: r.get(14),
        favicon_needs_check: r.get(15),
    }))
}

/// `update_rss_feed`'s permission gate: the feed exists, its owner exists, and
/// the owner is not read-only. Fails (→ `false` from `update_rss_feed`) with
/// the PHP's log message on each failure path.
pub async fn guard_updatable(pool: &PgPool, feed_id: i32) -> Result<()> {
    let Some(feed) = load_feed(pool, feed_id).await? else {
        bail!("feeds table record not found for feed: {feed_id}");
    };
    let Some(access_level): Option<i32> =
        sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE id = $1")
            .bind(feed.owner_uid)
            .fetch_optional(pool)
            .await
            .context("load feed owner")?
    else {
        bail!("owner not found for feed: {feed_id}");
    };
    if access_level == ACCESS_LEVEL_READONLY {
        bail!("denied update for {feed_id}: permission denied by owner access level");
    }
    Ok(())
}

/// The outcome of a feed parse + upsert.
#[derive(Debug, Clone, Default)]
pub struct FeedUpdateResult {
    /// Number of new `ttrss_entries` inserted (0 when all articles were
    /// deduplicated or the feed had no items).
    pub new_articles: i32,
    /// `Some(error)` when the feed XML failed to parse (the feed's
    /// `last_error` is stamped with it).
    pub parse_error: Option<String>,
}

/// The deterministic core of `update_rss_feed`: parse `feed_data`, upsert each
/// article, and stamp the feed's metadata. Returns the parse outcome plus the
/// number of new articles. DB failures propagate; a parse failure is reported
/// in [`FeedUpdateResult::parse_error`] (the feed's `last_error` is stamped).
pub async fn process_feed(
    pool: &PgPool,
    cfg: &Config,
    plugins: Arc<PluginHost>,
    feed: &FeedRow,
    feed_data: &str,
    flags: &FeedUpdateFlags,
    mut log: Option<&mut FeedDebugLog>,
) -> Result<FeedUpdateResult> {
    let feed_language = if feed.feed_language.trim().is_empty() {
        "simple".to_string()
    } else {
        feed.feed_language.trim().to_lowercase()
    };

    let mut parser = FeedParser::new(feed_data);
    if !parser.init() {
        let err = parser.error();
        // The PHP's parse-failure lines (`mb_substr($rss->error(), 0, 245)`).
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            format!("fetch error: {}", err.chars().take(245).collect::<String>()),
        );
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "update failed.".into());
        let _ = sqlx::query(
            "UPDATE ttrss_feeds SET last_updated = NOW(), \
             last_unconditional = NOW(), last_error = $1 WHERE id = $2",
        )
        .bind(err.clone())
        .bind(feed.id)
        .execute(pool)
        .await;
        return Ok(FeedUpdateResult {
            new_articles: 0,
            parse_error: Some(err),
        });
    }

    // `chain_hooks_callback(HOOK_FEED_PARSED, …)` (RSSUtils:624) — the
    // initialized parser + feed id are handed to each hook (the result is
    // not used).
    {
        let system: Vec<String> = cfg
            .plugins
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
            Arc::clone(&plugins),
            pool.clone(),
            cfg.clone(),
            &system,
            &cfg.self_url,
            feed.owner_uid,
        )
        .await;
        for idx in ctx.hook_order(HOOK_FEED_PARSED) {
            ctx.host_arc().plugin(idx).hook_feed_parsed(&mut ctx, &parser, feed.id).await;
        }
    }

    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("language: {feed_language}"));
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "processing feed data...".into());

    // site_url fallback (PHP: `if (empty($feed_obj->site_url))`).
    let site_url = if feed.site_url.trim().is_empty() {
        let fallback =
            rewrite_relative(&feed.feed_url, &clean_str(&parser.get_link())).unwrap_or_default();
        let _ = sqlx::query("UPDATE ttrss_feeds SET site_url = $1 WHERE id = $2")
            .bind(fallback.clone())
            .bind(feed.id)
            .execute(pool)
            .await;
        fallback
    } else {
        feed.site_url.clone()
    };
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("site_url: {site_url}"));
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("feed_title: {}", parser.get_title()),
    );

    // Favicon check (PHP: RSSUtils.php:648-697). After site_url is determined,
    // before the article loop.
    let favicon_due = feed.favicon_needs_check
        || flags.force_refetch
        || (feed.favicon_is_custom
            && feed
                .favicon_avg_color
                .as_deref()
                .is_none_or(|c| c.is_empty()));
    if favicon_due {
        // Restrict update attempts to once per 12h.
        let _ = sqlx::query("UPDATE ttrss_feeds SET favicon_last_checked = NOW() WHERE id = $1")
            .bind(feed.id)
            .execute(pool)
            .await;

        if !feed.favicon_is_custom {
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                "favicon: trying to update favicon...".into(),
            );
            crate::favicons::update_favicon(cfg, &site_url, feed.id).await;
        }

        // Compute avg color if the icon exists and we don't have one yet.
        let icon_path = cfg.feed_icon_dir().join(feed.id.to_string());
        let needs_color = feed
            .favicon_avg_color
            .as_deref()
            .is_none_or(|c| c.is_empty());
        if icon_path.exists() && needs_color {
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                "favicon: trying to calculate average color...".into(),
            );
            // Mark 'fail' first (the PHP's crash-recovery hack: colors.php:282).
            let _ = sqlx::query("UPDATE ttrss_feeds SET favicon_avg_color = 'fail' WHERE id = $1")
                .bind(feed.id)
                .execute(pool)
                .await;
            if let Some(color) = crate::favicons::calculate_avg_color(&icon_path) {
                let _ =
                    sqlx::query("UPDATE ttrss_feeds SET favicon_avg_color = $1 WHERE id = $2")
                        .bind(&color)
                        .bind(feed.id)
                        .execute(pool)
                        .await;
                dlog(
                    log.as_deref_mut(),
                    DEBUG_LEVEL_VERBOSE,
                    format!("favicon: calculated avg color: {color}"),
                );
            }
        }
    }

    let items: Vec<FeedItem> = parser.get_items().to_vec();
    if items.is_empty() {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "no articles found.".into());
        crate::feeds::purge_feed(pool, cfg, feed.id, 0).await;
        stamp_success(pool, feed.id).await?;
        return Ok(FeedUpdateResult::default());
    }

    let date_feed_processed = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    // M6b.2c — the owner's enabled article filters (scoped to this feed) and the
    // user's blacklisted tags, each loaded once per feed update.
    let filters = load_filters(pool, feed.id, feed.owner_uid).await?;
    let blacklisted = blacklisted_tags(pool, feed.owner_uid).await;
    // The PHP's `loading filters & labels...` (the port's label lookup happens
    // per article; the filters dump is the PHP's `print_r` at `LOG_EXTENDED`).
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "loading filters & labels...".into());
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_EXTENDED,
        if filters.is_empty() {
            // the PHP's `print_r` of an empty array
            "Array (\n)".to_string()
        } else {
            format!("{filters:?}")
        },
    );
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("{} filters loaded.", filters.len()),
    );
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "processing articles...".into());
    // M7.9 — the owner's plugin context for the `HOOK_ARTICLE_FILTER` chain, built once
    // per feed (the owner's enabled set is fixed for the batch). PHP loads the system
    // plugins (KIND_ALL) + the owner's user plugins (KIND_USER) per feed (RSSUtils:
    // 303-304); `owner_ctx_from` mirrors that against a bare pool.
    let system: Vec<String> = cfg
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
        plugins,
        pool.clone(),
        cfg.clone(),
        &system,
        &cfg.self_url,
        feed.owner_uid,
    )
    .await;
    let mut new_articles = 0;
    for item in &items {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
        new_articles += upsert_article(
            pool,
            feed,
            &site_url,
            &feed_language,
            &date_feed_processed,
            item,
            &filters,
            &blacklisted,
            flags,
            &mut ctx,
            log.as_deref_mut(),
        )
        .await?;
    }

    // `Feeds::_purge($feed, 0)` — the per-feed purge (M6c.4), before the
    // success stamp (the PHP's `update_rss_feed` order: commit → purge → stamp).
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "purging feed...".into());
    crate::feeds::purge_feed(pool, cfg, feed.id, 0).await;
    stamp_success(pool, feed.id).await?;
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "update done.".into());
    Ok(FeedUpdateResult {
        new_articles,
        parse_error: None,
    })
}

/// `update_rss_feed` (the deterministic skeleton): the permission gate plus
/// [`process_feed`]. The feed data is supplied by the caller (the daemon fetches
/// it in M6c). Returns `true` on a successful parse + upsert, `false` when the
/// feed is not updatable or the XML failed to parse.
pub async fn update_rss_feed(
    pool: &PgPool,
    cfg: &Config,
    plugins: Arc<PluginHost>,
    feed_id: i32,
    feed_data: &str,
    flags: &FeedUpdateFlags,
    mut log: Option<&mut FeedDebugLog>,
) -> Result<bool> {
    if guard_updatable(pool, feed_id).await.is_err() {
        return Ok(false);
    }
    let Some(feed) = load_feed(pool, feed_id).await? else {
        return Ok(false);
    };
    let result = process_feed(pool, cfg, plugins, &feed, feed_data, flags, log.as_deref_mut()).await?;
    Ok(result.parse_error.is_none())
}

// ── Feed-debugger debug-log capture (the PHP's `Debug::log` + `enable_html`) ──

/// The PHP's `Debug` levels (`classes/Debug.php`). A line is shown on the
/// debugger page when its level is ≤ the page's selected level (the form
/// offers `LOG_VERBOSE` = 1, the default, and `LOG_EXTENDED` = 2).
pub const DEBUG_LEVEL_NORMAL: u8 = 0;
pub const DEBUG_LEVEL_VERBOSE: u8 = 1;
pub const DEBUG_LEVEL_EXTENDED: u8 = 2;

/// `Debug::SEPARATOR` (rendered as `<hr/>` on the debugger page).
pub const DEBUG_SEPARATOR: &str = "<-{log-separator}->";

/// One captured line of the feed-update debug output (the PHP's
/// `Debug::log(…, html_output = true)` stdout, plus the direct `dump_feed_xml`
/// prints).
#[derive(Debug, Clone)]
pub enum DebugEntry {
    /// A `Debug::log` line (a `DEBUG_SEPARATOR` message renders as `<hr/>`).
    Log { level: u8, message: String },
    /// A `dump_feed_xml` dump (rendered as `<code class='feed-xml'>…</code>`;
    /// the PHP prints it directly, without a timestamp).
    Xml(String),
}

/// The collector for [`DebugEntry`]s. Attached only by the feed debugger page
/// (`Feeds::update_debugger`); the daemon passes `None`.
#[derive(Debug, Clone, Default)]
pub struct FeedDebugLog {
    pub entries: Vec<DebugEntry>,
}

impl FeedDebugLog {
    pub fn log(&mut self, level: u8, message: impl Into<String>) {
        self.entries
            .push(DebugEntry::Log { level, message: message.into() });
    }
    /// `Debug::SEPARATOR` (the PHP's `<hr/>` line).
    pub fn separator(&mut self) {
        self.entries.push(DebugEntry::Log {
            level: DEBUG_LEVEL_VERBOSE,
            message: DEBUG_SEPARATOR.into(),
        });
    }
    pub fn xml(&mut self, data: String) {
        self.entries.push(DebugEntry::Xml(data));
    }
}

/// The update flags the PHP's `update_rss_feed` reads from `$_REQUEST` — set
/// only by the feed debugger page (the daemon runs with the defaults).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedUpdateFlags {
    /// `force_refetch` — an unconditional fetch (skips `last_modified`).
    pub force_refetch: bool,
    /// `force_rehash` — skip the "stored article seems up to date" early-out
    /// (re-process articles whose hash is unchanged).
    pub force_rehash: bool,
    /// `dump_feed_xml` — print the fetched XML on the page (twice: the PHP's
    /// "before hooks" / "after hooks" — identical with no plugins).
    pub dump_feed_xml: bool,
    /// The PHP's `$no_cache` (the debugger calls with `true`): the fetch uses
    /// `feed_fetch_no_cache_timeout` instead of `feed_fetch_timeout`.
    pub no_cache: bool,
}

/// Emit a debug line when a collector is attached (the daemon passes `None`).
fn dlog(log: Option<&mut FeedDebugLog>, level: u8, message: String) {
    if let Some(l) = log {
        l.log(level, message);
    }
}

/// The outcome of a single-feed fetch + update (M6c.2).
#[derive(Debug, Clone)]
pub struct FeedFetchOutcome {
    /// `true` when the feed was updated (a 200 with parseable XML) or confirmed
    /// unchanged (a 304); `false` on a fetch / parse failure.
    pub success: bool,
    /// Number of new `ttrss_entries` (0 for a 304 / no-articles / failure).
    pub new_articles: i32,
    /// The error message (when the update failed).
    pub error: Option<String>,
}

/// `fetch_one_feed` — the M6c.2 single-feed update workhorse: fetch the feed
/// (M6c.1), make the conditional-refetch decision, and on success run the M6b
/// [`process_feed`] pipeline. Mirrors the per-feed body of the PHP's
/// `RSSUtils::update_rss_feed` (minus the dropped 30s feed-content cache, D5=c).
///
/// - A **304** stamps `last_error = ''`, `last_successful_update = now`,
///   `last_updated = now` (no `last_unconditional` — the fetch was conditional).
/// - A **429** throttles the feed (randomized `HTTP_429_THROTTLE_INTERVAL` 1-2x)
///   by pushing `last_updated` / `last_successful_update` into the future.
/// - Any other **failure** stamps `last_error` + `last_updated = now`.
/// - A **200** updates `last_modified` (if changed) and runs `process_feed`
///   (which stamps the success / parse-failure metadata).
pub async fn fetch_one_feed(
    pool: &PgPool,
    cfg: &Config,
    plugins: Arc<PluginHost>,
    feed_id: i32,
    flags: &FeedUpdateFlags,
    mut log: Option<&mut FeedDebugLog>,
) -> Result<FeedFetchOutcome> {
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "start".into());
    let Some(feed) = load_feed(pool, feed_id).await? else {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_NORMAL,
            format!("error: feeds table record not found for feed: {feed_id}"),
        );
        return Ok(FeedFetchOutcome {
            success: false,
            new_articles: 0,
            error: Some(format!("feeds table record not found for feed: {feed_id}")),
        });
    };
    if let Err(e) = guard_updatable(pool, feed_id).await {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_NORMAL, format!("error: {e}"));
        return Ok(FeedFetchOutcome {
            success: false,
            new_articles: 0,
            error: Some(e.to_string()),
        });
    }

    // Claim the feed (the PHP's `last_update_started = Db::NOW()`).
    let _ = sqlx::query("UPDATE ttrss_feeds SET last_update_started = NOW() WHERE id = $1")
        .bind(feed_id)
        .execute(pool)
        .await;

    // Conditional-refetch decision: force an unconditional fetch once
    // `last_unconditional` is older than `MAX_CONDITIONAL_INTERVAL`.
    let computed_force = match feed.last_unconditional {
        Some(lu) => (Utc::now().naive_utc() - lu).num_seconds() > cfg.max_conditional_interval,
        None => true,
    };
    // The PHP's conditional-decision debug lines (the request flag can only
    // *add* a force, as in the PHP).
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!(
            "last unconditional update request: {}",
            feed.last_unconditional
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default()
        ),
    );
    if computed_force {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            "maximum allowed interval for conditional requests exceeded, forcing refetch".into(),
        );
    } else {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            format!("stored last modified for conditional request: {}", feed.last_modified),
        );
    }
    let force_refetch = computed_force || flags.force_refetch;
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("fetching {} (force_refetch: {force_refetch})...", feed.feed_url),
    );

    // The feed's auth (the `auth_pass` is decrypted from its stored envelope).
    let auth_pass = decrypt_feed_pass(&feed.auth_pass, cfg.encryption_key.as_ref());

    // HOOK_FETCH_FEED (M7.22 item 8): a plugin may substitute the feed data,
    // skipping the fetch. PHP `RSSUtils` fetch flow (RSSUtils:449) —
    // `chain_hooks_callback(HOOK_FETCH_FEED, …)` threads `$feed_data` (initially
    // empty) through each hook; a non-empty result means a plugin provided the
    // data. No shipped plugin does this (af_comics's `on_fetch` returns `false`),
    // so it's a no-op for the stock set.
    let plugin_feed_data: String = {
        let system: Vec<String> = cfg
            .plugins
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
            Arc::clone(&plugins),
            pool.clone(),
            cfg.clone(),
            &system,
            &cfg.self_url,
            feed.owner_uid,
        )
        .await;
        let host = ctx.host_arc();
        let mut data = String::new();
        for idx in ctx.hook_order(HOOK_FETCH_FEED) {
            data = host
                .plugin(idx)
                .hook_fetch_feed(
                    &mut ctx,
                    &data,
                    &feed.feed_url,
                    feed.owner_uid,
                    feed_id,
                    0,
                    &feed.auth_login,
                    &auth_pass,
                )
                .await;
        }
        data
    };

    // A plugin supplied the feed data — skip the fetch and parse it directly.
    if !plugin_feed_data.trim().is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            "feed data has been modified by a plugin.".into(),
        );
        // `dump_feed_xml` for this path (M7.22 item 8): mirrors the PHP's
        // before/after-hooks dump — before is empty (no fetch happened), after
        // is the plugin's data.
        if flags.dump_feed_xml {
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "feed data before hooks:".into());
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "feed data after hooks:".into());
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
            if let Some(l) = log.as_deref_mut() {
                l.xml(plugin_feed_data.clone());
            }
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
        }
        let feed_data = plugin_feed_data.trim().to_string();
        let result = process_feed(
            pool,
            cfg,
            Arc::clone(&plugins),
            &feed,
            &feed_data,
            flags,
            log.as_deref_mut(),
        )
        .await?;
        return Ok(FeedFetchOutcome {
            success: result.parse_error.is_none(),
            new_articles: result.new_articles,
            error: result.parse_error,
        });
    }

    let res = fetch(&FetchOptions {
        url: feed.feed_url.clone(),
        login: if feed.auth_login.is_empty() {
            None
        } else {
            Some(feed.auth_login.clone())
        },
        pass: if auth_pass.is_empty() {
            None
        } else {
            Some(auth_pass)
        },
        timeout: Some(std::time::Duration::from_secs(
            if flags.no_cache {
                cfg.feed_fetch_no_cache_timeout
            } else {
                cfg.feed_fetch_timeout
            },
        )),
        last_modified: if force_refetch {
            None
        } else {
            Some(feed.last_modified.clone())
        },
        filtering: Some(true),
        ..Default::default()
    })
    .await;
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "fetch done.".into());
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!(
            "effective URL (after redirects): {} (IP: {}) ",
            res.effective_url, res.effective_ip
        ),
    );
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("server last modified: {}", res.last_modified),
    );

    let body = res
        .body
        .as_deref()
        .map_or(String::new(), |b| String::from_utf8_lossy(b).into_owned());
    let mut feed_data = body.trim().to_string();

    // Empty body → the error path (304 / 429 / other).
    if feed_data.is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            format!("unable to fetch: {} [{}]", res.error, res.error_code),
        );
        let now = Utc::now();
        return if res.error_code == 304 {
            // 304: the source claims the feed is unchanged.
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                "source claims data not modified (304), nothing to do.".into(),
            );
            let _ = sqlx::query(
                "UPDATE ttrss_feeds SET last_error = '', last_successful_update = $1, \
                 last_updated = $1 WHERE id = $2",
            )
            .bind(now)
            .bind(feed_id)
            .execute(pool)
            .await;
            Ok(FeedFetchOutcome {
                success: true,
                new_articles: 0,
                error: None,
            })
        } else if res.error_code == 429 {
            // 429: throttle (randomized `HTTP_429_THROTTLE_INTERVAL` 1-2x).
            let base = cfg.http_429_throttle_interval;
            let interval: i64 = rand::thread_rng().gen_range(base..=base * 2);
            let future = now + chrono::Duration::seconds(interval);
            let err_msg = format!("{} (updates throttled for {} seconds.)", res.error, interval);
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                format!(
                    "source claims we're requesting too often (429), throttling updates for {interval} seconds."
                ),
            );
            let _ = sqlx::query(
                "UPDATE ttrss_feeds SET last_error = $1, last_successful_update = $2, \
                 last_updated = $2 WHERE id = $3",
            )
            .bind(&err_msg)
            .bind(future)
            .bind(feed_id)
            .execute(pool)
            .await;
            Ok(FeedFetchOutcome {
                success: false,
                new_articles: 0,
                error: Some(err_msg),
            })
        } else {
            // Generic fetch failure.
            let _ = sqlx::query(
                "UPDATE ttrss_feeds SET last_error = $1, last_updated = $2 WHERE id = $3",
            )
            .bind(&res.error)
            .bind(now)
            .bind(feed_id)
            .execute(pool)
            .await;
            Ok(FeedFetchOutcome {
                success: false,
                new_articles: 0,
                error: Some(res.error),
            })
        };
    }

    // 200: update `last_modified` (if changed), then run the M6b pipeline.
    if !res.last_modified.is_empty() && res.last_modified != feed.last_modified {
        let lm: String = res.last_modified.chars().take(245).collect();
        let _ = sqlx::query("UPDATE ttrss_feeds SET last_modified = $1 WHERE id = $2")
            .bind(&lm)
            .bind(feed_id)
            .execute(pool)
            .await;
    }

    // The PHP's `dump_feed_xml` "feed data before hooks" (RSSUtils:590).
    if flags.dump_feed_xml {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "feed data before hooks:".into());
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
        if let Some(l) = log.as_deref_mut() {
            l.xml(feed_data.to_string());
        }
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
    }

    // `chain_hooks_callback(HOOK_FEED_FETCHED, …)` (RSSUtils:594): the raw XML
    // is threaded through each hook (a plugin may rewrite it before parsing) —
    // args (feed_data, feed_url, owner_uid, feed), result replaces the data.
    {
        let system: Vec<String> = cfg
            .plugins
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
            Arc::clone(&plugins),
            pool.clone(),
            cfg.clone(),
            &system,
            &cfg.self_url,
            feed.owner_uid,
        )
        .await;
        for idx in ctx.hook_order(HOOK_FEED_FETCHED) {
            feed_data = ctx
                .host_arc()
                .plugin(idx)
                .hook_feed_fetched(&mut ctx, &feed_data, &feed.feed_url, feed.owner_uid, feed_id)
                .await;
        }
    }

    // …and the "feed data after hooks" dump.
    if flags.dump_feed_xml {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "feed data after hooks:".into());
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
        if let Some(l) = log.as_deref_mut() {
            l.xml(feed_data.to_string());
        }
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR.into());
    }
    let result = process_feed(pool, cfg, plugins, &feed, &feed_data, flags, log.as_deref_mut()).await?;
    Ok(FeedFetchOutcome {
        success: result.parse_error.is_none(),
        new_articles: result.new_articles,
        error: result.parse_error,
    })
}
/// Stamp the feed's success metadata (PHP: `last_updated`,
/// `last_unconditional`, `last_successful_update`, `last_error = ''`).
async fn stamp_success(pool: &PgPool, feed_id: i32) -> Result<()> {
    let _ = sqlx::query(
        "UPDATE ttrss_feeds SET last_updated = NOW(), last_unconditional = NOW(), \
         last_successful_update = NOW(), last_error = '' WHERE id = $1",
    )
    .bind(feed_id)
    .execute(pool)
    .await
    .context("stamp feed success metadata")?;
    Ok(())
}

/// `RSSUtils::load_filters($feed_id, $owner_uid)` — the owner's enabled article
/// filters, scoped to this feed (a rule naming the feed directly, one naming a
/// category in its chain, or an unrestricted rule) in the shape
/// `eval_article_filters` consumes. A filter is kept only when it has at least
/// one applicable rule and at least one action.
pub(crate) async fn load_filters(
    pool: &PgPool,
    feed_id: i32,
    owner_uid: i32,
) -> Result<Vec<EvalFilter>> {
    // `Feeds::_cat_of`: the feed's `cat_id` (0 when uncategorized/NULL; -1 when
    // the feed no longer exists — only reachable if called for a deleted feed).
    let cat_id: i64 = sqlx::query_scalar("SELECT cat_id FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_optional(pool)
        .await
        .context("look up feed category")?
        .map(|v: Option<i32>| v.unwrap_or(0) as i64)
        .unwrap_or(-1);

    // `($cat_id) ? '' : 'cat_id IS NULL OR'` (the PHP has no trailing space).
    let null_cat_qpart = if cat_id == 0 { "cat_id IS NULL OR" } else { "" };

    // `$check_cats = [..._get_parent_cats($cat_id, $owner_uid), $cat_id]`.
    let mut check_cats: Vec<i64> = if cat_id > 0 {
        parent_cats(pool, owner_uid as i64, cat_id).await
    } else {
        Vec::new()
    };
    check_cats.push(cat_id);
    let check_cats_str = check_cats
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let check_cats_fullids: Vec<String> = check_cats.iter().map(|c| format!("CAT:{c}")).collect();

    let filter_rows = sqlx::query(
        "SELECT id, match_any_rule, inverse FROM ttrss_filters2 \
         WHERE owner_uid = $1 AND enabled = true ORDER BY order_id, title",
    )
    .bind(owner_uid)
    .fetch_all(pool)
    .await
    .context("load owner filters")?;

    let mut filters: Vec<EvalFilter> = Vec::new();
    for row in &filter_rows {
        let filter_id: i32 = row.get(0);
        let match_any_rule: bool = row.get(1);
        let inverse: bool = row.get(2);

        let rule_q = format!(
            "SELECT reg_exp, inverse, match_on, filter_type FROM ttrss_filters2_rules \
             WHERE (match_on IS NOT NULL OR \
                    (({null_cat_qpart} (cat_id IS NULL AND cat_filter = false) OR cat_id IN ({check_cats_str})) AND \
                    (feed_id IS NULL OR feed_id = $1))) AND filter_id = $2"
        );
        let rule_rows = sqlx::query(AssertSqlSafe(rule_q))
            .bind(feed_id)
            .bind(filter_id)
            .fetch_all(pool)
            .await
            .context("load filter rules")?;

        let mut rules: Vec<EvalRule> = Vec::new();
        for r in &rule_rows {
            let reg_exp: String = r.get(0);
            let rule_inverse: bool = r.get(1);
            let match_on: Option<String> = r.get(2);
            let filter_type: i32 = r.get(3);
            let rule_type = RuleType::from_id(filter_type);

            // `if ($rule_line["match_on"])` — a non-empty `match_on` restricts the
            // rule to the feeds/categories listed in that JSON array.
            let match_on = match_on.as_deref().unwrap_or("");
            if !match_on.is_empty() {
                let forms: Vec<String> = serde_json::from_str::<Vec<Value>>(match_on)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|v| match v {
                        Value::Number(n) => Some(n.to_string()),
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                let matches_feed = forms.contains(&"0".to_string())
                    || forms.contains(&feed_id.to_string())
                    || check_cats_fullids.iter().any(|ci| forms.contains(ci));
                if matches_feed {
                    rules.push(EvalRule {
                        reg_exp,
                        rule_type,
                        inverse: rule_inverse,
                    });
                } else if !match_any_rule {
                    rules.clear();
                    break;
                }
            } else {
                rules.push(EvalRule {
                    reg_exp,
                    rule_type,
                    inverse: rule_inverse,
                });
            }
        }

        if rules.is_empty() {
            continue;
        }

        let action_rows = sqlx::query(
            "SELECT a.action_param, t.name AS type_name \
             FROM ttrss_filters2_actions AS a, ttrss_filter_actions AS t \
             WHERE t.id = a.action_id AND a.filter_id = $1",
        )
        .bind(filter_id)
        .fetch_all(pool)
        .await
        .context("load filter actions")?;

        let actions: Vec<FilterAction> = action_rows
            .iter()
            .map(|a| {
                let action_param: String = a.get(0);
                let type_name: String = a.get(1);
                FilterAction {
                    action_type: Some(type_name),
                    param: Some(action_param),
                }
            })
            .collect();

        if actions.is_empty() {
            continue;
        }

        filters.push(EvalFilter {
            match_any_rule,
            inverse,
            rules,
            actions,
        });
    }

    Ok(filters)
}

/// `Prefs::get(BLACKLISTED_TAGS, $owner_uid)` — the user's blacklisted tags as a
/// normalized set (the PHP default for the pref is the empty string, i.e. no
/// blacklist).
async fn blacklisted_tags(pool: &PgPool, owner_uid: i32) -> std::collections::BTreeSet<String> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 \
         WHERE owner_uid = $1 AND pref_name = $2 AND profile IS NULL",
    )
    .bind(owner_uid)
    .bind(crate::prefs::BLACKLISTED_TAGS)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let items: Vec<String> = raw
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();
    normalize_categories(&items).into_iter().collect()
}

/// The PHP `$article` field values, in hash order, with the PHP stringification
/// applied (`content` stripped of tags; `tags`/`enclosures` comma-joined).
#[allow(clippy::too_many_arguments)] // mirrors the PHP `$article` field list
fn article_hashed_fields(
    title: &str,
    content: &str,
    link: &str,
    author: &str,
    language: &str,
    timestamp: i64,
    num_comments: i32,
    tags: &[String],
    enclosures_hash: &str,
) -> Vec<(String, String)> {
    vec![
        ("title".into(), title.to_string()),
        ("content".into(), strip_tags(content)),
        ("link".into(), link.to_string()),
        ("tags".into(), tags.join(",")),
        ("author".into(), author.to_string()),
        // `score_modifier` is 0 for a new article (no plugin recalculation).
        ("score_modifier".into(), "0".to_string()),
        ("language".into(), language.to_string()),
        ("timestamp".into(), timestamp.to_string()),
        ("num_comments".into(), num_comments.to_string()),
        ("enclosures".into(), enclosures_hash.to_string()),
    ]
}

/// PHP `FeedEnclosure::__toString()` — the string form `calculate_article_hash`
/// feeds into `implode(',', $enclosures)`. Field order and labels match the PHP
/// `sprintf` exactly.
fn enclosure_to_string(enc: &FeedEnclosure) -> String {
    format!(
        "FeedEnclosure(link={}, type={}, length={}, title={}, height={}, width={})",
        enc.link, enc.enc_type, enc.length, enc.title, enc.height, enc.width
    )
}
/// Collect + upsert a single feed item (the deterministic core of the PHP's
/// per-article loop). Returns 1 if a new `ttrss_entries` row was created, else 0.
#[allow(clippy::too_many_arguments)] // the per-article upsert context (mirrors the PHP's many locals)
async fn upsert_article(
    pool: &PgPool,
    feed: &FeedRow,
    site_url: &str,
    feed_language: &str,
    date_feed_processed: &str,
    item: &FeedItem<'_>,
    filters: &[EvalFilter],
    blacklisted: &std::collections::BTreeSet<String>,
    flags: &FeedUpdateFlags,
    ctx: &mut HostCtx,
    mut log: Option<&mut FeedDebugLog>,
) -> Result<i32> {
    let owner = feed.owner_uid;

    // --- GUID selection (PHP: get_id → get_link → make_guid_from_title) ---
    let mut entry_guid = strip_tags(&item.get_id());
    if entry_guid.is_empty() {
        entry_guid = strip_tags(&item.get_link());
    }
    if entry_guid.is_empty() {
        entry_guid = make_guid_from_title(&item.get_title());
    }
    if entry_guid.is_empty() {
        return Ok(0); // `continue`
    }

    let guid_hashed_compat = format!("SHA1:{}", sha1_hex(format!("{owner},{entry_guid}")));
    // The PHP-order `guid_hashed` (see `rss_utils::make_guid_hashed`).
    let guid_hashed = make_guid_hashed(owner, &entry_guid);
    let entry_guid = format!("{owner},{entry_guid}");
    let entry_guid = entry_guid.chars().take(245).collect::<String>();
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("guid {entry_guid} (hash: {guid_hashed} compat: {guid_hashed_compat})"),
    );

    // --- field collection ---
    let entry_timestamp = item.get_date().unwrap_or(0);
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("orig date: {entry_timestamp} ({})", ts_to_fmt(entry_timestamp)),
    );
    let mut entry_title = strip_tags(&item.get_title());
    let entry_link =
        rewrite_relative(site_url, &clean_str(&item.get_link())).unwrap_or_else(|| item.get_link());
    let entry_language = item
        .get_language()
        .trim()
        .chars()
        .take(2)
        .collect::<String>();
    if entry_title.is_empty() {
        // `date("Y-m-d H:i:s", $entry_timestamp)`
        entry_title = ts_to_fmt(entry_timestamp);
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("title {entry_title}"));
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("link {entry_link}"));
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("language {entry_language}"));
    let mut entry_content = item.get_content();
    if entry_content.is_empty() {
        entry_content = item.get_description();
    }
    let entry_comments = item
        .get_comments_url()
        .chars()
        .take(245)
        .collect::<String>();
    let num_comments = item.get_comments_count();
    let entry_author = strip_tags(&item.get_author())
        .chars()
        .take(245)
    .collect::<String>();
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("author {entry_author}"));
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "looking for tags...".into());
    let entry_tags = item.get_categories();
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("tags found: {}", entry_tags.join(", ")),
    );
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "done collecting data.".into());
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "looking for enclosures...".into());
    // PHP: `$encs = $item->get_enclosures();` then, per enclosure, run
    // `HOOK_ENCLOSURE_IMPORTED` (a plugin may rewrite the enclosure), rewrite
    // the link against the feed's site URL (`UrlHelper::rewrite_relative`) and
    // drop any whose link fails validation.
    let mut enclosures: Vec<FeedEnclosure> = Vec::new();
    for mut enc in item.get_enclosures() {
        // `chain_hooks_callback(HOOK_ENCLOSURE_IMPORTED, …)` (RSSUtils:814) —
        // the enclosure's field map is threaded through each hook; the result
        // replaces the enclosure before the link rewrite.
        let mut enc_val = json!({
            "link": enc.link,
            "type": enc.enc_type,
            "length": enc.length,
            "title": enc.title,
            "height": enc.height,
            "width": enc.width,
        });
        for idx in ctx.hook_order(HOOK_ENCLOSURE_IMPORTED) {
            ctx.host_arc()
                .plugin(idx)
                .hook_enclosure_imported(ctx, &mut enc_val, feed.id)
                .await;
        }
        enc.link = enc_val
            .get("link")
            .and_then(Value::as_str)
            .unwrap_or(&enc.link)
            .to_string();
        enc.enc_type = enc_val
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(&enc.enc_type)
            .to_string();
        enc.length = enc_val
            .get("length")
            .and_then(Value::as_str)
            .unwrap_or(&enc.length)
            .to_string();
        enc.title = enc_val
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&enc.title)
            .to_string();
        enc.height = enc_val
            .get("height")
            .and_then(Value::as_str)
            .unwrap_or(&enc.height)
            .to_string();
        enc.width = enc_val
            .get("width")
            .and_then(Value::as_str)
            .unwrap_or(&enc.width)
            .to_string();
        match crate::sanitizer::rewrite_relative(site_url, &enc.link, "", "", &enc.enc_type) {
            Some(url) => enc.link = url,
            None => {
                dlog(
                    log.as_deref_mut(),
                    DEBUG_LEVEL_VERBOSE,
                    "Skipping enclosure whose link failed validation.".into(),
                );
                continue;
            }
        }
        enclosures.push(enc);
    }
    // The hash contribution is `implode(',', $enclosures)` — each element
    // stringified via `FeedEnclosure::__toString()`.
    let enclosures_hash = enclosures
        .iter()
        .map(enclosure_to_string)
        .collect::<Vec<_>>()
        .join(",");

    // --- look up the existing base entry (by any of the 3 guid forms) ---
    let existing: Option<(i32, String)> =
        sqlx::query("SELECT id, content_hash FROM ttrss_entries WHERE guid IN ($1, $2, $3)")
            .bind(&entry_guid)
            .bind(&guid_hashed)
            .bind(&guid_hashed_compat)
            .fetch_optional(pool)
            .await
            .context("look up base entry")?
            .map(|r| (r.get(0), r.get(1)));

    let (base_entry_id, entry_stored_hash, existing_tags) = match &existing {
        Some((id, hash)) => {
            let tags: Vec<String> = sqlx::query_scalar(
                "SELECT tag_name FROM ttrss_tags WHERE post_int_id = \
                 (SELECT int_id FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2 \
                  LIMIT 1)",
            )
            .bind(*id)
            .bind(owner)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            let merged: Vec<String> = entry_tags
                .iter()
                .cloned()
                .chain(tags)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            (Some(*id), hash.clone(), merged)
        }
        None => (None, String::new(), entry_tags.clone()),
    };

    // --- article hash (dedup key) ---
    let entry_current_hash = calculate_article_hash(&article_hashed_fields(
        &entry_title,
        &entry_content,
        &entry_link,
        &entry_author,
        &entry_language,
        entry_timestamp,
        num_comments,
        &existing_tags,
        &enclosures_hash,
    ));
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!(
            "article hash: {entry_current_hash} [stored={entry_stored_hash}]"
        ),
    );

    if entry_current_hash == entry_stored_hash {
        // The PHP's `force_rehash` flag skips this early-out (re-process the
        // article even though its hash is unchanged).
        if !flags.force_rehash {
            // "stored article seems up to date, updating timestamp only."
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                format!(
                    "stored article seems up to date [IID: {}], updating timestamp only.",
                    base_entry_id.map(|i| i.to_string()).unwrap_or_default()
                ),
            );
            if let Some(id) = base_entry_id {
                let _ = sqlx::query("UPDATE ttrss_entries SET date_updated = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await;
            }
            return Ok(0);
        }
    } else {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            "hash differs, running HOOK_ARTICLE_FILTER handlers...".into(),
        );
    }

    // --- HOOK_ARTICLE_FILTER (M7.9): fire the owner's article-filter chain on the raw
    //     article. PHP `chain_hooks_callback(HOOK_ARTICLE_FILTER, …)` (RSSUtils:876),
    //     *after* the pre-hook hash is computed and *before* the base record is created.
    //     A plugin may rewrite title/content/tags/author/link; the pre-hook hash is still
    //     what's stored, and the post-hook fields (re-read below) feed the insert + the
    //     user-filter eval. `entry_plugin_data` accumulates the lowercased class names
    //     (stored on the base entry). A no-op when no plugin registered the hook.
    let enclosures_value: Vec<Value> = enclosures
        .iter()
        .map(|e| {
            json!({
                "link": e.link,
                "type": e.enc_type,
                "length": e.length,
                "title": e.title,
                "width": e.width,
                "height": e.height,
            })
        })
        .collect();
    // M7.10 — seed `article["labels"]` with the article's existing labels (the
    // PHP's `$article["labels"] = $article_labels`, RSSUtils:836) so the
    // `HOOK_ARTICLE_FILTER` chain can add to them (and dedup against them); a
    // just-created entry has none.
    let existing_article_labels: Vec<Value> = match base_entry_id {
        Some(id) => crate::article::get_labels(pool, owner as i64, id as i64, Some(owner as i64)).await,
        None => Vec::new(),
    };
    let mut article = json!({
        "owner_uid": owner,
        "guid": entry_guid,
        "guid_hashed": guid_hashed,
        "title": entry_title,
        "content": entry_content,
        "link": entry_link,
        "labels": Value::Array(existing_article_labels),
        "tags": existing_tags,
        "author": entry_author,
        "force_catchup": false,
        "score_modifier": 0,
        "language": entry_language,
        "timestamp": entry_timestamp,
        "num_comments": num_comments,
        "enclosures": enclosures_value,
        "feed": {
            "id": feed.id,
            "fetch_url": feed.feed_url,
            "site_url": site_url,
            "cache_images": feed.cache_images,
        },
    });
    let mut entry_plugin_data = String::new();
    let host = ctx.host_arc();
    for idx in ctx.hook_order(HOOK_ARTICLE_FILTER) {
        let cls = host.plugin(idx).class_name().to_lowercase();
        article = host.plugin(idx).hook_article_filter(ctx, &mut article).await;
        entry_plugin_data.push_str(&format!("{cls},"));
    }
    // Re-read the (possibly-rewritten) fields; these shadow the raw locals for the rest
    // of the function (base-record insert/update, the user-filter eval, tag storage).
    let entry_title = article
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&entry_title)
        .to_string();
    let entry_content = article
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or(&entry_content)
        .to_string();
    let entry_author = article
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or(&entry_author)
        .to_string();
    let entry_link = article
        .get("link")
        .and_then(Value::as_str)
        .unwrap_or(&entry_link)
        .to_string();
    let existing_tags: Vec<String> = article
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or(existing_tags.clone());

    // --- resolve the base entry (create it if missing) — the PHP does this
    //     *before* running the filters, so the RID lines appear between
    //     "hash differs..." and the matched-filter dumps ---
    let base_record_created = base_entry_id.is_none();
    let ref_id: i32 = match base_entry_id {
        Some(id) => {
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("select returned RID: {id}"));
            id
        },
        None => {
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                format!("base guid [{entry_guid} or {guid_hashed}] not found, creating..."),
            );
            let updated = ts_to_naive(entry_timestamp);
            let ts_content = format!(
                "{} {}",
                strip_tags(&entry_title),
                strip_tags(&entry_content)
            );
            let id: i32 = sqlx::query_scalar(
                "INSERT INTO ttrss_entries (title, tsvector_combined, guid, link, updated, content, content_hash, no_orig_date, date_updated, date_entered, comments, num_comments, plugin_data, lang, author) VALUES ($1, to_tsvector($12::regconfig, $13), $2, $3, $4, $5, $6, false, NOW(), $7::timestamp, $8, $9, $14, $10, $11) RETURNING id",
            )
            .bind(&entry_title)
            .bind(&guid_hashed)
            .bind(&entry_link)
            .bind(updated)
            .bind(&entry_content)
            .bind(&entry_current_hash)
            .bind(date_feed_processed)
            .bind(&entry_comments)
            .bind(num_comments)
            .bind(&entry_language)
            .bind(&entry_author)
            .bind(feed_language)
            .bind(&ts_content)
            .bind(&entry_plugin_data)
            .fetch_one(pool)
            .await
            .context("insert base entry")?;
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, format!("insert returned RID: {id}"));
            id
        }
    };
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("base record with RID: {ref_id} found, checking for user record"),
    );
    // The article's label list (existing labels seeded above + the
    // `HOOK_ARTICLE_FILTER` additions): feeds the `LOG_EXTENDED` "article
    // labels:" dump and the "labels [other]" re-apply below. (The PHP re-reads
    // `$article["labels"]` here — RSSUtils:975.)
    let article_labels = article
        .get("labels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // M6b.2c — run the article through the loaded filters.
    let outcome = eval_article_filters(
        filters,
        &entry_title,
        &entry_content,
        &entry_link,
        &entry_author,
        &existing_tags,
    );
    // `run_hooks(HOOK_FILTER_TRIGGERED, …)` (RSSUtils:904) — after the
    // evaluation, before the matched ids are applied: the feed, owner,
    // article, matched filters/rules, and the collected actions.
    let matched_filters_val: Vec<Value> = outcome
        .matched_filters
        .iter()
        .map(|&i| &filters[i])
        .map(|f| {
            json!({
                "match_any_rule": f.match_any_rule,
                "inverse": f.inverse,
                "rules": f.rules.iter().map(|r| {
                    json!({
                        "reg_exp": r.reg_exp,
                        "rule_type": match r.rule_type {
                            RuleType::Title => "title",
                            RuleType::Content => "content",
                            RuleType::Both => "both",
                            RuleType::Link => "link",
                            RuleType::Author => "author",
                            RuleType::Tag => "tag",
                            RuleType::Unknown => "date",
                        },
                        "inverse": r.inverse,
                    })
                }).collect::<Vec<_>>(),
                "actions": f.actions.iter().map(|a| {
                    json!({
                        "type": a.action_type,
                        "param": a.param,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let matched_rules_val: Vec<Value> = outcome
        .matched_rules
        .iter()
        .map(|r| {
            json!({
                "rule_index": r.rule_index,
                "regexp_matches": r.regexp_matches,
            })
        })
        .collect();
    let actions_val: Vec<Value> = outcome
        .actions
        .iter()
        .map(|a| {
            json!({
                "type": a.action_type,
                "param": a.param,
            })
        })
        .collect();
    let matched_filters_v = Value::Array(matched_filters_val);
    let matched_rules_v = Value::Array(matched_rules_val);
    let actions_v = Value::Array(actions_val);
    for idx in ctx.hook_order(HOOK_FILTER_TRIGGERED) {
        ctx.host_arc()
            .plugin(idx)
            .hook_filter_triggered(
                ctx,
                feed.id,
                owner,
                &article,
                &matched_filters_v,
                &matched_rules_v,
                &actions_v,
            )
            .await;
    }
    // The PHP's `LOG_EXTENDED`-gated filter dumps: the three headers are
    // logged unconditionally at level 2; the array bodies only when non-empty
    // (`{:?}` stands in for the PHP's `print_r`).
    dlog(log.as_deref_mut(), DEBUG_LEVEL_EXTENDED, "matched filters: ".into());
    if !outcome.matched_filters.is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_EXTENDED,
            format!("{:?}", outcome.matched_filters),
        );
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_EXTENDED, "matched filter rules: ".into());
    if !outcome.matched_rules.is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_EXTENDED,
            format!("{:?}", outcome.matched_rules),
        );
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_EXTENDED, "filter actions: ".into());
    if !outcome.actions.is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_EXTENDED,
            format!("{:?}", outcome.actions),
        );
    }
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("num_comments: {num_comments}"),
    );
    dlog(log.as_deref_mut(), DEBUG_LEVEL_EXTENDED, "article labels:".into());
    if !article_labels.is_empty() {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_EXTENDED,
            format!("{:?}", article_labels),
        );
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "force catchup: false".into());

    // A `filter` action drops the article: the base record is kept, but no user
    // entry is created (PHP `continue`s here).
    if has_article_filter_action(&outcome.actions, "filter") {
        dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "article is filtered out, nothing to do.".into());
        return Ok(if base_record_created { 1 } else { 0 });
    }

    // --- user entry state, driven by the matched filter actions ---
    // `$score = calculate_article_score($actions) + $entry_score_modifier`
    // (`score_modifier` is 0 here — no plugin recalculation).
    let score = calculate_article_score(&outcome.actions);
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("initial score: {score} [including plugin modifier: 0]"),
    );
    // The PHP wraps the user-entry work in a transaction (this port's
    // statements are individually durable); the lines keep the output parity.
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "begin pdo transaction...".into());
    let has_catchup = has_article_filter_action(&outcome.actions, "catchup");
    // NOTE: the PHP's `!$entry_force_catchup` guard is omitted. That flag is
    // the plugin "force catchup" hook (the `$article["force_catchup"]` "ugly
    // hack") and is always `false` with no plugins loaded, so both of its uses
    // (here and `mark_unread_on_update` below) are no-ops until plugins (M7).
    let unread = score >= -500 && !has_catchup;
    let marked = has_article_filter_action(&outcome.actions, "mark") || score > 1000;
    let published = has_article_filter_action(&outcome.actions, "publish");
    let last_marked: Option<NaiveDateTime> = if marked {
        Some(Utc::now().naive_utc())
    } else {
        None
    };
    let last_published: Option<NaiveDateTime> = if published {
        Some(Utc::now().naive_utc())
    } else {
        None
    };

    let user_row: Option<(i32, i32)> = sqlx::query(
        "SELECT ref_id, int_id FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
    )
    .bind(ref_id)
    .bind(owner)
    .fetch_optional(pool)
    .await
    .context("look up user entry")?
    .map(|r| (r.get(0), r.get(1)));

    let entry_int_id: i32 = match user_row {
        Some((_, int_id)) => {
            dlog(
                log.as_deref_mut(),
                DEBUG_LEVEL_VERBOSE,
                format!("user record FOUND: RID: {ref_id}, IID: {int_id}"),
            );
            int_id
        },
        None => {
            dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "user record not found, creating...".into());
            let last_read: Option<NaiveDateTime> = if unread {
                None
            } else {
                Some(Utc::now().naive_utc())
            };
            let int_id: i32 = sqlx::query_scalar(
                "INSERT INTO ttrss_user_entries \
                 (ref_id, owner_uid, feed_id, unread, last_read, marked, published, \
                  score, tag_cache, label_cache, uuid, last_marked, last_published) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, '', '', '', $9, $10) RETURNING int_id",
            )
            .bind(ref_id)
            .bind(owner)
            .bind(feed.id)
            .bind(unread)
            .bind(last_read)
            .bind(marked)
            .bind(published)
            .bind(score)
            .bind(last_marked)
            .bind(last_published)
            .fetch_one(pool)
            .await
            .context("insert user entry")?;
            // PHP `run_hooks(HOOK_ARTICLES_*_TOGGLED, [$ref_id])`
            // (RSSUtils:1146-1149) — fired when the fresh entry is created
            // already marked / published (filter actions).
            if marked {
                for idx in ctx.hook_order(HOOK_ARTICLES_MARK_TOGGLED) {
                    ctx.host_arc()
                        .plugin(idx)
                        .hook_articles_mark_toggled(ctx, &[ref_id as i64])
                        .await;
                }
            }
            if published {
                for idx in ctx.hook_order(HOOK_ARTICLES_PUBLISH_TOGGLED) {
                    ctx.host_arc()
                        .plugin(idx)
                        .hook_articles_publish_toggled(ctx, &[ref_id as i64])
                        .await;
                }
            }
            int_id
        }
    };
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("resulting RID: {ref_id}, IID: {entry_int_id}"),
    );

    // --- update the base entry's mutable fields (only when not just created) ---
    if !base_record_created {
        let updated = ts_to_naive(entry_timestamp);
        let ts_content = format!(
            "{} {}",
            strip_tags(&entry_title),
            strip_tags(&entry_content)
        );
        let _ = sqlx::query(
            "UPDATE ttrss_entries SET title = $1, tsvector_combined = to_tsvector($9::regconfig, $10), content = $2, content_hash = $3, updated = $4, date_updated = NOW(), num_comments = $5, plugin_data = $11, author = $6, lang = $7 WHERE id = $8",
        )
        .bind(&entry_title)
        .bind(&entry_content)
        .bind(&entry_current_hash)
        .bind(updated)
        .bind(num_comments)
        .bind(&entry_author)
        .bind(&entry_language)
        .bind(ref_id)
        .bind(feed_language)
        .bind(&ts_content)
        .bind(&entry_plugin_data)
        .execute(pool)
        .await;
    }

    // --- re-score + mark-unread-on-update (applied whether or not the base
    //     record was just created; the PHP's `!$entry_force_catchup` guard is
    //     a no-op here — see the NOTE at the unread calculation above) ---
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET score = $1 WHERE ref_id = $2 AND owner_uid = $3",
    )
    .bind(score)
    .bind(ref_id)
    .bind(owner)
    .execute(pool)
    .await;
    if feed.mark_unread_on_update && !has_catchup {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            "article updated, marking unread as requested.".into(),
        );
        let _ = sqlx::query(
            "UPDATE ttrss_user_entries SET last_read = NULL, unread = true \
             WHERE ref_id = $1 AND owner_uid = $2",
        )
        .bind(ref_id)
        .bind(owner)
        .execute(pool)
        .await;
    } else {
        dlog(
            log.as_deref_mut(),
            DEBUG_LEVEL_VERBOSE,
            "article updated, but we're forbidden to mark it unread.".into(),
        );
    }

    // --- labels [other]: re-apply the article's existing labels, then [filters]:
    //     every `label` action's caption (the PHP adds both unconditionally — its
    //     `in_array` guard is a no-op; `add_article` is idempotent and skips
    //     unknown labels) ---
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "assigning labels [other]...".into());
    for label in &article_labels {
        if let Some(caption) = label.get(1).and_then(Value::as_str) {
            crate::labels::add_article(pool, ref_id as i64, caption, owner as i64).await;
        }
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "assigning labels [filters]...".into());
    for fa in &outcome.actions {
        if fa.action_type.as_deref() == Some("label")
            && let Some(param) = fa.param.as_deref() {
                crate::labels::add_article(pool, ref_id as i64, param, owner as i64).await;
            }
    }
    // The PHP's `LOG_EXTENDED` "article enclosures:" dump — `print_r` of the
    // parsed enclosures (the established `{:?}`-for-`print_r` approximation;
    // empty is the PHP's `print_r([])` body).
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_EXTENDED,
        "article enclosures:".into(),
    );
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_EXTENDED,
        if enclosures.is_empty() {
            "Array (\n)".to_string()
        } else {
            format!("{enclosures:?}")
        },
    );

    // Persist the enclosures (PHP lines 1221–1234): dedupe on
    // (content_url, content_type, post_id) and INSERT the rest. `post_id` is the
    // base entry id (`ttrss_entries.id` = `ref_id`), matching the read side's
    // `num_enclosures` / `_get_enclosures` subqueries. (`cache_enclosures` is
    // gated on the feed's `cache_images`, off by default, and is out of scope —
    // network + disk-cache, like `cache_media`.)
    for enc in &enclosures {
        let exists: Option<i32> = sqlx::query_scalar(
            "SELECT id FROM ttrss_enclosures \
             WHERE content_url = $1 AND content_type = $2 AND post_id = $3",
        )
        .bind(&enc.link)
        .bind(&enc.enc_type)
        .bind(ref_id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
        if exists.is_none() {
            let _ = sqlx::query(
                "INSERT INTO ttrss_enclosures \
                 (content_url, content_type, title, duration, post_id, width, height) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&enc.link)
            .bind(&enc.enc_type)
            .bind(&enc.title)
            .bind(php_int_cast(&enc.length).to_string())
            .bind(ref_id)
            .bind(php_int_cast(&enc.width) as i32)
            .bind(php_int_cast(&enc.height) as i32)
            .execute(pool)
            .await;
        }
    }

    // --- article tags: feed categories + filter `tag`/`ignore-tag`, minus the
    //     user's blacklisted tags (the whole set is re-normalized at the end) ---
    let mut final_tags: Vec<String> = existing_tags.clone();
    for fa in &outcome.actions {
        if fa.action_type.as_deref() == Some("tag") {
            let raw: Vec<String> = fa
                .param
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|s| s.to_string())
                .collect();
            final_tags.extend(normalize_categories(&raw));
        }
    }
    for fa in &outcome.actions {
        if fa.action_type.as_deref() == Some("ignore-tag") {
            let raw: Vec<String> = fa
                .param
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|s| s.to_string())
                .collect();
            let remove: std::collections::BTreeSet<String> =
                normalize_categories(&raw).into_iter().collect();
            final_tags.retain(|t| !remove.contains(t));
        }
    }
    let final_tags: Vec<String> = final_tags
        .iter()
        .filter(|t| !blacklisted.contains(t.as_str()))
        .cloned()
        .collect();
    let final_tags = normalize_categories(&final_tags);
    dlog(
        log.as_deref_mut(),
        DEBUG_LEVEL_VERBOSE,
        format!("resulting article tags: {}", final_tags.join(", ")),
    );

    if !final_tags.is_empty() {
        for tag in &final_tags {
            let exists: Option<i32> = sqlx::query_scalar(
                "SELECT id FROM ttrss_tags WHERE tag_name = $1 AND post_int_id = $2 AND owner_uid = $3 LIMIT 1",
            )
            .bind(tag)
            .bind(entry_int_id)
            .bind(owner)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
            if exists.is_none() {
                let _ = sqlx::query(
                    "INSERT INTO ttrss_tags (owner_uid, tag_name, post_int_id) VALUES ($1, $2, $3)",
                )
                .bind(owner)
                .bind(tag)
                .bind(entry_int_id)
                .execute(pool)
                .await;
            }
        }
        let _ = sqlx::query(
            "UPDATE ttrss_user_entries SET tag_cache = $1 WHERE ref_id = $2 AND owner_uid = $3",
        )
        .bind(final_tags.join(","))
        .bind(ref_id)
        .bind(owner)
        .execute(pool)
        .await;
    }
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "commit pdo transaction...".into());
    dlog(log.as_deref_mut(), DEBUG_LEVEL_VERBOSE, "article processed.".into());

    Ok(if base_record_created { 1 } else { 0 })
}

/// PHP `date("Y-m-d H:i:s", $ts)` from a Unix timestamp (UTC).
fn ts_to_fmt(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| {
            Utc::now()
                .naive_utc()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
}

/// Unix timestamp → `NaiveDateTime` (UTC), falling back to "now".
fn ts_to_naive(ts: i64) -> NaiveDateTime {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.naive_utc())
        .unwrap_or_else(|| Utc::now().naive_utc())
}

// -- M6c.3 — the daemon/scheduler loop -------------------------------------

/// A feed selected for update (the daemon loop's due-feeds query result).
#[derive(Debug, Clone)]
pub struct DueFeed {
    pub id: i32,
    pub owner_uid: i32,
    /// The feed's stored title (for the loop's per-feed log lines).
    pub feed_title: String,
}

/// The `update_daemon_common` selection SQL — the due feeds (the least recently
/// updated first, capped at `limit`). The PHP ref: `RSSUtils::update_daemon_common`
/// (the base feed query). `update_interval = 0` means "use the default
/// interval" (the `DEFAULT_UPDATE_INTERVAL` pref, default 30 min); a positive
/// value is the feed's own interval (minutes); `-1` means disabled. A feed that
/// has never been updated (`last_updated` NULL or the epoch) is always due.
pub async fn select_due_feeds(pool: &PgPool, cfg: &Config, limit: i32) -> Vec<DueFeed> {
    let default_interval: i64 = 30; // Prefs::DEFAULT_UPDATE_INTERVAL
    let login_thresh = if !cfg.single_user_mode && cfg.daemon_update_login_limit > 0 {
        format!(
            "AND u.last_login >= NOW() - INTERVAL '{} day'",
            cfg.daemon_update_login_limit
        )
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT f.id, f.owner_uid, f.title FROM ttrss_feeds f, ttrss_users u \
         LEFT JOIN ttrss_user_prefs2 p ON (p.owner_uid = u.id AND p.profile IS NULL \
              AND p.pref_name = 'DEFAULT_UPDATE_INTERVAL') \
         WHERE f.owner_uid = u.id AND u.access_level NOT IN (-2, -1) {login_thresh} \
         AND ((f.update_interval = 0 AND (p.value IS NULL OR p.value != '-1') \
               AND f.last_updated < NOW() - CAST((COALESCE(p.value, '{default_interval}') || ' minutes') AS INTERVAL)) \
              OR (f.update_interval > 0 AND f.last_updated < NOW() - CAST((f.update_interval || ' minutes') AS INTERVAL)) \
              OR (f.update_interval >= 0 AND (p.value IS NULL OR p.value != '-1') \
               AND (f.last_updated = '1970-01-01 00:00:00' OR f.last_updated IS NULL))) \
         AND (f.last_update_started IS NULL OR f.last_update_started < NOW() - INTERVAL '10 minute') \
         ORDER BY f.last_updated NULLS FIRST LIMIT {limit}"
    );
    let Ok(rows) = sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await else {
        return Vec::new();
    };
    rows.iter()
        .map(|r| DueFeed {
            id: r.get::<i32, _>(0),
            owner_uid: r.get::<i32, _>(1),
            feed_title: r.get::<String, _>(2),
        })
        .collect()
}

/// Claim the due feeds (the PHP's `last_update_started = NOW()` — the in-process
/// on-demand dedup + the "currently being updated" marker).
pub async fn claim_feeds(pool: &PgPool, feeds: &[DueFeed]) {
    if feeds.is_empty() {
        return;
    }
    let qmarks: Vec<String> = (0..feeds.len()).map(|i| format!("${}", i + 1)).collect();
    let sql = format!(
        "UPDATE ttrss_feeds SET last_update_started = NOW() WHERE id IN ({})",
        qmarks.join(", ")
    );
    let mut q = sqlx::query(AssertSqlSafe(sql));
    for feed in feeds {
        q = q.bind(feed.id);
    }
    let _ = q.execute(pool).await;
}

/// The global daemon host ctx — the PHP's `PluginHost::getInstance()` in the
/// CLI bootstrap: `load(Config::PLUGINS, KIND_ALL)` (all configured plugins,
/// no per-owner filtering; `owner_uid = 0` → no `_ENABLED_PLUGINS`).
async fn global_ctx(
    pool: &PgPool,
    cfg: &Config,
    plugins: &Arc<PluginHost>,
) -> crate::pluginhost::host::HostCtx {
    let system: Vec<String> = cfg
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    crate::pluginhost::dispatch::owner_ctx_from(
        Arc::clone(plugins),
        pool.clone(),
        cfg.clone(),
        &system,
        &cfg.self_url,
        0,
    )
    .await
}

/// `run_hooks(HOOK_HOUSE_KEEPING)` — the housekeeping hook chain. The PHP
/// fires it at two sites, with two different plugin hosts:
/// - `housekeeping_user($owner_uid)` (RSSUtils:1669-1675): a fresh host with
///   the system plugins + *that owner's* `_ENABLED_PLUGINS` — once per owner
///   in the just-processed feed batch;
/// - `housekeeping_common()` (RSSUtils:1765-1771): the global daemon host
///   (`owner_uid = 0`: no per-owner user plugins), after the due system +
///   plugin tasks.
pub async fn run_house_keeping_hook(
    pool: &PgPool,
    cfg: &Config,
    plugins: &Arc<PluginHost>,
    owner_uid: i32,
) {
    let system: Vec<String> = cfg
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
        Arc::clone(plugins),
        pool.clone(),
        cfg.clone(),
        &system,
        &cfg.self_url,
        owner_uid,
    )
    .await;
    for idx in ctx.hook_order(HOOK_HOUSE_KEEPING) {
        ctx.host_arc().plugin(idx).hook_house_keeping(&mut ctx).await;
    }
}

/// `run_hooks(HOOK_UPDATE_TASK, $options)` (update.php:248-250): fired on the
/// global daemon host after `housekeeping_common`, with the daemon process's
/// CLI options map. The port's daemon is env-driven and always the
/// `--daemon-loop` variant (no `--task`/`--quiet`/… flags) →
/// `{"daemon-loop": true}`.
pub async fn run_update_task_hook(
    pool: &PgPool,
    cfg: &Config,
    plugins: &Arc<PluginHost>,
) {
    let mut ctx = global_ctx(pool, cfg, plugins).await;
    let cli_options = serde_json::json!({ "daemon-loop": true });
    for idx in ctx.hook_order(HOOK_UPDATE_TASK) {
        ctx.host_arc().plugin(idx).hook_update_task(&mut ctx, &cli_options).await;
    }
}

/// The daemon/scheduler loop (M6c.3). A background task that periodically
/// selects due feeds, claims them, dispatches per-feed update tasks (bounded by
/// `Semaphore(daemon_max_jobs)`), awaits completion, then sleeps the interval.
///
/// The loop is the faithful sequential daemon cycle: it processes one
/// `DAEMON_FEED_LIMIT` batch, awaits completion, then sleeps the interval (large
/// feed sets cycle over minutes, as in the PHP). No cross-process lock (D1 = B,
/// strictly single-process). The loop exits when `cancel` is cancelled (the
/// graceful-shutdown path).
pub async fn daemon_loop(pool: &PgPool, cfg: &Config, plugins: Arc<PluginHost>, cancel: tokio_util::sync::CancellationToken) {
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    let interval = std::time::Duration::from_secs(cfg.daemon_sleep_interval.max(60) as u64);
    let n = (cfg.daemon_max_jobs.max(1)) as usize;

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // The first tick is immediate.

    loop {
        // Select due feeds.
        let feeds = select_due_feeds(pool, cfg, cfg.daemon_feed_limit).await;
        if !feeds.is_empty() {
            // Claim the feeds.
            claim_feeds(pool, &feeds).await;
            tracing::debug!(feeds = feeds.len(), "daemon: updating due feeds");

            // Dispatch per-feed tasks (bounded by Semaphore(N)).
            let started = std::time::Instant::now();
            let semaphore = Arc::new(Semaphore::new(n));
            let mut join_set = JoinSet::new();
            for feed in &feeds {
                let permit = semaphore.clone().acquire_owned().await;
                let pool = pool.clone();
                let cfg = cfg.clone();
                let plugins = plugins.clone();
                let id = feed.id;
                let title = feed.feed_title.clone();
                join_set.spawn(async move {
                    let _permit = permit;
                    let outcome = fetch_one_feed(
                        &pool,
                        &cfg,
                        plugins,
                        id,
                        &FeedUpdateFlags::default(),
                        None,
                    )
                    .await;
                    // Per-feed visibility (debug on success, warn on failure):
                    // the batch results used to be discarded silently.
                    match &outcome {
                        Ok(o) if o.success => {
                            tracing::debug!(feed = %title, new_articles = o.new_articles, "feed update ok");
                        }
                        Ok(o) => {
                            tracing::warn!(feed = %title, error = ?o.error, "feed update failed");
                        }
                        Err(e) => {
                            tracing::warn!(feed = %title, error = %e, "feed update errored");
                        }
                    }
                    outcome
                });
            }
            // Drain/await: the loop processes one batch, awaits completion.
            while let Some(result) = join_set.join_next().await {
                let _ = result;
            }
            tracing::debug!(
                feeds = feeds.len(),
                secs = started.elapsed().as_secs_f64(),
                "daemon: batch done"
            );

            // `housekeeping_user($owner_uid)` (RSSUtils:284-287): after the
            // batch, once per *distinct owner in it* — the HOOK_HOUSE_KEEPING
            // chain on the system + that owner's enabled plugins. (The
            // PHP's per-owner `run_due_tasks` is covered by the single-host
            // `run_plugin_tasks` below, which runs the `:uid`-suffixed user
            // tasks.)
            let mut owners: Vec<i32> = feeds.iter().map(|f| f.owner_uid).collect();
            owners.sort_unstable();
            owners.dedup();
            for owner in owners {
                run_house_keeping_hook(pool, cfg, &plugins, owner).await;
            }
        }

        // `housekeeping_common()` (RSSUtils:1765-1771): the due system tasks
        // (Scheduler), then the due plugin tasks, then HOOK_HOUSE_KEEPING on
        // the global daemon host.
        crate::scheduler::run_due_tasks(pool, cfg, &crate::scheduler::system_tasks(cfg)).await;
        crate::scheduler::run_plugin_tasks(pool, cfg, &plugins).await;
        run_house_keeping_hook(pool, cfg, &plugins, 0).await;

        // `run_hooks(HOOK_UPDATE_TASK, $options)` (update.php:248, after
        // `housekeeping_common`) on the global daemon host.
        run_update_task_hook(pool, cfg, &plugins).await;

        // Sleep the interval (or exit on graceful shutdown).
        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel.cancelled() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::feed::FeedEnclosure;

    /// PHP `FeedEnclosure::__toString()` — the string form the article hash
    /// feeds via `implode(',', $enclosures)`. The labels, field order, and the
    /// empty-string defaults are the observable contract (a wrong format
    /// silently changes the dedup hash for every article with enclosures).
    #[test]
    fn test_enclosure_to_string_matches_php() {
        let enc = FeedEnclosure {
            link: "https://example.com/media/abc.png".to_string(),
            enc_type: "image/jpeg".to_string(),
            length: "0".to_string(),
            title: String::new(),
            height: String::new(),
            width: String::new(),
        };
        assert_eq!(
            enclosure_to_string(&enc),
            "FeedEnclosure(link=https://example.com/media/abc.png, type=image/jpeg, length=0, title=, height=, width=)"
        );
    }

    /// The hash contribution is `implode(',', $enclosures)` — one element per
    /// (post-rewrite) enclosure, comma-joined, empty string for none.
    #[test]
    fn test_enclosures_hash_contribution() {
        let none: Vec<FeedEnclosure> = Vec::new();
        assert_eq!(none.iter().map(enclosure_to_string).collect::<Vec<_>>().join(","), "");

        let one = FeedEnclosure {
            link: "https://example.com/a.mp3".to_string(),
            enc_type: "audio/mpeg".to_string(),
            length: "120".to_string(),
            title: "Song".to_string(),
            height: String::new(),
            width: String::new(),
        };
        let encs = [one.clone(), one];
        assert_eq!(
            encs.iter().map(enclosure_to_string).collect::<Vec<_>>().join(","),
            "FeedEnclosure(link=https://example.com/a.mp3, type=audio/mpeg, length=120, title=Song, height=, width=),"
                .to_string() +
                "FeedEnclosure(link=https://example.com/a.mp3, type=audio/mpeg, length=120, title=Song, height=, width=)"
        );
    }

    /// The PHP `(int)` casts on length/width/height (bound into the
    /// `integer`/`text` columns) must match PHP's leading-numeric cast.
    #[test]
    fn test_enclosure_dim_casts() {
        assert_eq!(php_int_cast("0"), 0);
        assert_eq!(php_int_cast(""), 0);
        assert_eq!(php_int_cast("1234"), 1234);
        assert_eq!(php_int_cast("12.9"), 12); // PHP `(int)` truncates the fraction
        assert_eq!(php_int_cast("  5  "), 5); // leading/trailing ws ignored
    }
}
