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

use anyhow::{bail, Context, Result};
use chrono::{NaiveDateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Row};

use crate::core::feed::{FeedItem, FeedParser};
use crate::core::user::ACCESS_LEVEL_READONLY;
use crate::rss_utils::{calculate_article_hash, make_guid_from_title};
use crate::urlhelper::rewrite_relative;
use crate::util::{clean_str, sha1_hex, strip_tags};

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
}

/// Load the feed row, or `None` if the id does not exist.
pub async fn load_feed(pool: &PgPool, feed_id: i32) -> Result<Option<FeedRow>> {
    let row = sqlx::query(
        "SELECT id, owner_uid, title, feed_url, site_url, feed_language, \
         mark_unread_on_update, cache_images, purge_interval \
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
        stamp_success(pool, feed.id).await?;
        return Ok(FeedUpdateResult::default());
    }

    let date_feed_processed = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let mut new_articles = 0;
    for item in &items {
        new_articles += upsert_article(
            pool,
            feed,
            &site_url,
            &feed_language,
            &date_feed_processed,
            item,
        )
        .await?;
    }

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
pub async fn update_rss_feed(pool: &PgPool, feed_id: i32, feed_data: &str) -> Result<bool> {
    if guard_updatable(pool, feed_id).await.is_err() {
        return Ok(false);
    }
    let Some(feed) = load_feed(pool, feed_id).await? else {
        return Ok(false);
    };
    let result = process_feed(pool, &feed, feed_data).await?;
    Ok(result.parse_error.is_none())
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
async fn upsert_article(
    pool: &PgPool,
    feed: &FeedRow,
    site_url: &str,
    feed_language: &str,
    date_feed_processed: &str,
    item: &FeedItem<'_>,
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

    // --- user entry (score/marked/published are filter-driven; no filters →
    //     score 0 → unread, unmarked, unpublished) ---
    let score: i64 = 0;
    let unread = score >= -500; // no catchup action, no force_catchup
    let marked = score > 1000; // no `mark` action
    let published = false; // no `publish` action
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

    // --- article tags (no filters → just the feed's categories) ---
    if !existing_tags.is_empty() {
        for tag in &existing_tags {
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
        .bind(existing_tags.join(","))
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
