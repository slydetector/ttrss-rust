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
//! - Favicon / `cache_images` / `cache_enclosures` (network + disk-cache) are
//!   M6c and are skipped here.
//! - The per-feed purge (`Feeds::_purge`, M6c.4) runs before the success
//!   stamp, matching the PHP's `update_rss_feed` order (commit → purge →
//!   stamp the success metadata).

use anyhow::{bail, Context, Result};
use chrono::{NaiveDateTime, Utc};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use rand::Rng;
use std::sync::Arc;

use crate::core::config::Config;
use crate::core::feed::{FeedItem, FeedParser};
use crate::core::filters::{
    calculate_article_score, eval_article_filters, has_article_filter_action, EvalFilter, EvalRule,
    FilterAction, RuleType,
};
use crate::core::user::ACCESS_LEVEL_READONLY;
use crate::counters::parent_cats;
use crate::feeds::decrypt_feed_pass;
use crate::rss_utils::{calculate_article_hash, make_guid_from_title};
use crate::urlhelper::{fetch, FetchOptions, rewrite_relative};
use crate::util::{clean_str, normalize_categories, sha1_hex, strip_tags};

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
}

/// Load the feed row, or `None` if the id does not exist.
pub async fn load_feed(pool: &PgPool, feed_id: i32) -> Result<Option<FeedRow>> {
    let row = sqlx::query(
        "SELECT id, owner_uid, title, feed_url, site_url, feed_language, \
         mark_unread_on_update, cache_images, purge_interval, \
         last_unconditional, last_modified, auth_login, auth_pass \
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
    feed: &FeedRow,
    feed_data: &str,
) -> Result<FeedUpdateResult> {
    let feed_language = if feed.feed_language.trim().is_empty() {
        "simple".to_string()
    } else {
        feed.feed_language.trim().to_lowercase()
    };

    let mut parser = FeedParser::new(feed_data);
    if !parser.init() {
        let err = parser.error();
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

    let items: Vec<FeedItem> = parser.get_items().to_vec();
    if items.is_empty() {
        crate::feeds::purge_feed(pool, cfg, feed.id, 0).await;
        stamp_success(pool, feed.id).await?;
        return Ok(FeedUpdateResult::default());
    }

    let date_feed_processed = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    // M6b.2c — the owner's enabled article filters (scoped to this feed) and the
    // user's blacklisted tags, each loaded once per feed update.
    let filters = load_filters(pool, feed.id, feed.owner_uid).await?;
    let blacklisted = blacklisted_tags(pool, feed.owner_uid).await;
    let mut new_articles = 0;
    for item in &items {
        new_articles += upsert_article(
            pool,
            feed,
            &site_url,
            &feed_language,
            &date_feed_processed,
            item,
            &filters,
            &blacklisted,
        )
        .await?;
    }

    // `Feeds::_purge($feed, 0)` — the per-feed purge (M6c.4), before the
    // success stamp (the PHP's `update_rss_feed` order: commit → purge → stamp).
    crate::feeds::purge_feed(pool, cfg, feed.id, 0).await;
    stamp_success(pool, feed.id).await?;
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
    feed_id: i32,
    feed_data: &str,
) -> Result<bool> {
    if guard_updatable(pool, feed_id).await.is_err() {
        return Ok(false);
    }
    let Some(feed) = load_feed(pool, feed_id).await? else {
        return Ok(false);
    };
    let result = process_feed(pool, cfg, &feed, feed_data).await?;
    Ok(result.parse_error.is_none())
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
pub async fn fetch_one_feed(pool: &PgPool, cfg: &Config, feed_id: i32) -> Result<FeedFetchOutcome> {
    let Some(feed) = load_feed(pool, feed_id).await? else {
        return Ok(FeedFetchOutcome {
            success: false,
            new_articles: 0,
            error: Some(format!("feeds table record not found for feed: {feed_id}")),
        });
    };
    if let Err(e) = guard_updatable(pool, feed_id).await {
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
    let force_refetch = match feed.last_unconditional {
        Some(lu) => (Utc::now().naive_utc() - lu).num_seconds() > cfg.max_conditional_interval,
        None => true,
    };

    // The feed's auth (the `auth_pass` is decrypted from its stored envelope).
    let auth_pass = decrypt_feed_pass(&feed.auth_pass, cfg.encryption_key.as_ref());

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
        timeout: Some(std::time::Duration::from_secs(cfg.feed_fetch_timeout)),
        last_modified: if force_refetch {
            None
        } else {
            Some(feed.last_modified.clone())
        },
        filtering: Some(cfg.fetch_extended_url_filtering),
        ..Default::default()
    })
    .await;

    let body = res
        .body
        .as_deref()
        .map_or(String::new(), |b| String::from_utf8_lossy(b).into_owned());
    let feed_data = body.trim();

    // Empty body → the error path (304 / 429 / other).
    if feed_data.is_empty() {
        let now = Utc::now();
        return if res.error_code == 304 {
            // 304: the source claims the feed is unchanged.
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

    let result = process_feed(pool, cfg, &feed, feed_data).await?;
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
        let rule_rows = sqlx::query(&rule_q)
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

    let guid_hashed_compat = format!("SHA1:{}", sha1_hex(&format!("{owner},{entry_guid}")));
    let guid_hashed = json!({
        "ver": 2,
        "uid": owner,
        "hash": format!("SHA1:{}", sha1_hex(&entry_guid)),
    })
    .to_string();
    let entry_guid = format!("{owner},{entry_guid}");
    let entry_guid = entry_guid.chars().take(245).collect::<String>();

    // --- field collection ---
    let entry_timestamp = item.get_date().unwrap_or(0);
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
    let entry_tags = item.get_categories();
    // No enclosures in this pass → the hash contribution is the empty string.
    let enclosures_hash = String::new();

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

    if entry_current_hash == entry_stored_hash {
        // "stored article seems up to date, updating timestamp only."
        if let Some(id) = base_entry_id {
            let _ = sqlx::query("UPDATE ttrss_entries SET date_updated = NOW() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await;
        }
        return Ok(0);
    }

    // M6b.2c — run the article through the loaded filters.
    let outcome = eval_article_filters(
        filters,
        &entry_title,
        &entry_content,
        &entry_link,
        &entry_author,
        &existing_tags,
    );

    // --- resolve the base entry (create it if missing) ---
    let base_record_created = base_entry_id.is_none();
    let ref_id: i32 = match base_entry_id {
        Some(id) => id,
        None => {
            let updated = ts_to_naive(entry_timestamp);
            let ts_content = format!(
                "{} {}",
                strip_tags(&entry_title),
                strip_tags(&entry_content)
            );
            let id: i32 = sqlx::query_scalar(
                "INSERT INTO ttrss_entries (title, tsvector_combined, guid, link, updated, content, content_hash, no_orig_date, date_updated, date_entered, comments, num_comments, plugin_data, lang, author) VALUES ($1, to_tsvector($12::regconfig, $13), $2, $3, $4, $5, $6, false, NOW(), $7::timestamp, $8, $9, '', $10, $11) RETURNING id",
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
            .fetch_one(pool)
            .await
            .context("insert base entry")?;
            id
        }
    };
    // A `filter` action drops the article: the base record is kept, but no user
    // entry is created (PHP `continue`s here).
    if has_article_filter_action(&outcome.actions, "filter") {
        return Ok(if base_record_created { 1 } else { 0 });
    }

    // --- user entry state, driven by the matched filter actions ---
    // `$score = calculate_article_score($actions) + $entry_score_modifier`
    // (`score_modifier` is 0 here — no plugin recalculation).
    let score = calculate_article_score(&outcome.actions);
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
        Some((_, int_id)) => int_id,
        None => {
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
            int_id
        }
    };

    // --- update the base entry's mutable fields (only when not just created) ---
    if !base_record_created {
        let updated = ts_to_naive(entry_timestamp);
        let ts_content = format!(
            "{} {}",
            strip_tags(&entry_title),
            strip_tags(&entry_content)
        );
        let _ = sqlx::query(
            "UPDATE ttrss_entries SET title = $1, tsvector_combined = to_tsvector($9::regconfig, $10), content = $2, content_hash = $3, updated = $4, date_updated = NOW(), num_comments = $5, author = $6, lang = $7 WHERE id = $8",
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
        let _ = sqlx::query(
            "UPDATE ttrss_user_entries SET last_read = NULL, unread = true \
             WHERE ref_id = $1 AND owner_uid = $2",
        )
        .bind(ref_id)
        .bind(owner)
        .execute(pool)
        .await;
    }

    // --- labels [other]: re-apply the article's existing labels, then [filters]:
    //     every `label` action's caption (the PHP adds both unconditionally — its
    //     `in_array` guard is a no-op; `add_article` is idempotent and skips
    //     unknown labels) ---
    let article_labels =
        crate::article::get_labels(pool, owner as i64, ref_id, Some(owner as i64)).await;
    for label in &article_labels {
        if let Some(caption) = label.get(1).and_then(Value::as_str) {
            crate::labels::add_article(pool, ref_id, caption, owner as i64).await;
        }
    }
    for fa in &outcome.actions {
        if fa.action_type.as_deref() == Some("label") {
            if let Some(param) = fa.param.as_deref() {
                crate::labels::add_article(pool, ref_id, param, owner as i64).await;
            }
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
}

/// The `update_daemon_common` selection SQL — the due feeds (the least recently
/// updated first, capped at `limit`). The PHP ref: `RSSUtils::update_daemon_common`
/// (the base feed query). `update_interval = 0` means "use the default
/// interval" (the `DEFAULT_UPDATE_INTERVAL` pref, default 30 min); `update_interval
/// > 0` means the feed's own interval (minutes); `update_interval = -1` means
/// disabled. A feed that has never been updated (`last_updated` NULL or the
/// epoch) is always due.
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
        "SELECT f.id, f.owner_uid FROM ttrss_feeds f, ttrss_users u \
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
    let Ok(rows) = sqlx::query(&sql).fetch_all(pool).await else {
        return Vec::new();
    };
    rows.iter()
        .map(|r| DueFeed {
            id: r.get::<i32, _>(0),
            owner_uid: r.get::<i32, _>(1),
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
    let mut q = sqlx::query(&sql);
    for feed in feeds {
        q = q.bind(feed.id);
    }
    let _ = q.execute(pool).await;
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
pub async fn daemon_loop(pool: &PgPool, cfg: &Config, cancel: tokio_util::sync::CancellationToken) {
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

            // Dispatch per-feed tasks (bounded by Semaphore(N)).
            let semaphore = Arc::new(Semaphore::new(n));
            let mut join_set = JoinSet::new();
            for feed in &feeds {
                let permit = semaphore.clone().acquire_owned().await;
                let pool = pool.clone();
                let cfg = cfg.clone();
                let id = feed.id;
                join_set.spawn(async move {
                    let _permit = permit;
                    fetch_one_feed(&pool, &cfg, id).await
                });
            }
            // Drain/await: the loop processes one batch, awaits completion.
            while let Some(result) = join_set.join_next().await {
                let _ = result;
            }
        }

        // Housekeeping ("task 0"): run the due scheduled tasks (the PHP's
        // `housekeeping_common`, invoked after each `update_daemon_common`
        // cycle). The per-owner variant (`housekeeping_user`) is M8 (plugins).
        crate::scheduler::run_due_tasks(pool, cfg, &crate::scheduler::system_tasks(cfg)).await;

        // Sleep the interval (or exit on graceful shutdown).
        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel.cancelled() => break,
        }
    }
}
