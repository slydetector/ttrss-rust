//! `public.php?op=rss` — syndicated feed output (`Handler_Public::rss`).
//!
//! PHP validates a per-feed/category access key against `ttrss_access_keys`,
//! then generates an Atom (or JSON) feed from the matching feed/category
//! articles. The Atom body is rendered by tt-rss's home-grown `MiniTemplator`
//! from `templates/generated_feed.txt`; the JSON variant is built inline.
//!
//! The Rust port renders the same Atom document with minijinja from
//! `templates/feed_atom.jinja` (a faithful transcription of
//! `generated_feed.txt` — the `feed_hub` block is never emitted on this
//! path, so it is dropped) and builds the JSON feed with serde_json.
//!
//! NOTE(port): the `q` search filter is not ported yet (see
//! `HeadlinesParams::search`) — it affects the feed title and the first-id
//! check, but does not filter rows.
//! NOTE(port): the plugin-feed branch of `generate_syndicated_feed`
//! (a plugin `get_headlines` handler for `PLUGIN_FEED_BASE_INDEX` feeds) is
//! not ported (no plugin feeds in this deployment).
//! TODO(port): `Config::get_version()` is a placeholder constant (PHP derives
//! it from the docroot git metadata).

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use chrono::{NaiveDateTime, SecondsFormat, Utc};
use minijinja::Environment;
use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::article;
use crate::headlines::{
    get_headlines_query, order_to_override_query, HeadlinesParams, FEED_PUBLISHED,
    FEED_STARRED, PLUGIN_FEED_BASE_INDEX,
};
use crate::labels::LABEL_BASE_INDEX;
use crate::prefs;
use crate::sanitizer;
use crate::util::{self, htmlspecialchars, strip_tags, truncate_string};
use crate::{diskcache, AppState, nocache_headers};

/// `Config::get_version()` — TODO(port): derive from the docroot git
/// metadata (PHP `Config::get_version()`), like the frozen reference
/// `26.08-2f7d831f`.
const VERSION: &str = "26.08-2f7d831f";

/// `$_REQUEST` params for `op=rss` (`Handler_Public::rss`).
pub struct RssRequest<'a> {
    /// `id` — feed (or category) id.
    pub id: Option<&'a str>,
    /// `key` — the access key.
    pub key: Option<&'a str>,
    /// `is_cat` (`Handler::_param_to_bool`).
    pub is_cat: bool,
    /// `limit` (`(int)clean(...)`).
    pub limit: i64,
    /// `offset` (`(int)clean(...)`).
    pub offset: i64,
    /// `q` — search filter.
    pub search: &'a str,
    /// `view-mode`.
    pub view_mode: &'a str,
    /// `order`.
    pub order: &'a str,
    /// `ts` — start timestamp.
    pub start_ts: &'a str,
    /// `format` (default `atom`).
    pub format: &'a str,
    /// `orig_guid` (default empty).
    pub orig_guid: &'a str,
    /// `noxml` — serve the Atom document as `text/plain`.
    pub noxml: bool,
}

/// `op=rss` handler.
pub async fn handle_rss(state: &AppState, req: &RssRequest<'_>) -> Response {
    let res = match lookup_access_key(state, req).await {
        None => forbidden(),
        Some(owner_uid) => {
            if req.format != "atom" && req.format != "json" {
                unknown_format(req.format)
            } else {
                generate_feed(state, owner_uid, req).await
            }
        }
    };
    let mut res = res;
    // public.php starts a session (nocache cache-limiter) unconditionally.
    nocache_headers(res.headers_mut());
    res
}

/// `Handler_Public::generate_syndicated_feed`.
async fn generate_feed(state: &AppState, owner_uid: i64, req: &RssRequest<'_>) -> Response {
    let pool = state.db.pool();
    let feed = req.id.unwrap_or("");
    let self_url = state.config.self_url_rtrim();

    // `if (!$limit) $limit = 60;`
    let limit = if req.limit == 0 { 60 } else { req.limit };

    let n_feed: Option<i64> = feed.trim().parse().ok();
    // NOTE(port): the plugin-feed branch (`!$is_cat && is_numeric($feed) &&
    // $feed < PLUGIN_FEED_BASE_INDEX && $feed > LABEL_BASE_INDEX` → a plugin
    // handler's `get_headlines`) is not ported — no plugin feeds exist in
    // this deployment. Kept as a named check for parity.
    let _plugin_feed = !req.is_cat
        && n_feed
            .is_some_and(|f| f < PLUGIN_FEED_BASE_INDEX && f > LABEL_BASE_INDEX);

    // `[$override_order, $skip_first_id_check] = Feeds::_order_to_override_query($order)`
    let (override_order, skip_first_id_check) = order_to_override_query(req.order);

    // Virtual-feed default ordering (PHP loose `$feed == <int>` compare).
    let override_order = if override_order.is_empty() {
        match (n_feed, req.is_cat) {
            (Some(FEED_PUBLISHED), false) => "last_published DESC",
            (Some(FEED_STARRED), false) => "last_marked DESC",
            _ => "date_entered DESC, updated DESC",
        }
    } else {
        &override_order
    };

    let params = HeadlinesParams {
        feed: feed.to_string(),
        limit,
        view_mode: req.view_mode.to_string(),
        cat_view: req.is_cat,
        offset: req.offset,
        since_id: 0,
        owner_uid,
        search: req.search.to_string(),
        // The syndication path never sets search_language (PHP default "").
        search_language: String::new(),
        override_order: override_order.to_string(),
        include_children: true,
        ignore_vfeed_group: true,
        start_ts: req.start_ts.to_string(),
        skip_first_id_check,
    };

    let q = get_headlines_query(&state.db, &params).await;

    // `Feeds::_get_access_key($feed, false, $owner_uid)` (get-or-create).
    let key = get_or_create_access_key(pool, feed, false, owner_uid).await;
    let feed_self_url = format!(
        "{self_url}/public.php?op=rss&id={feed}&key={}",
        key.unwrap_or_default()
    );

    // `if (!$feed_site_url) $feed_site_url = Config::get_self_url();`
    let mut feed_site_url = q.feed_site_url;
    if feed_site_url.is_empty() {
        feed_site_url = self_url.clone();
    }
    let feed_title = htmlspecialchars(&q.feed_title);

    // No session in this public context: `bw_limit`/`safe_mode` are unset;
    // `STRIP_IMAGES` resolves against the feed owner's default profile.
    let rows = prefs::load_rows(&state.db, owner_uid, None).await;
    let strip_images = prefs::resolve("STRIP_IMAGES", &rows, None).as_bool();
    let note_style = "color: #9a8c59; background-color: #fff7d5; border: 1px dashed #e7d796; padding: 5px; margin-bottom: 1em;";
    // `DiskCache::instance("images")` → `<CACHE_DIR>/images/`.
    let image_cache_dir = state
        .config
        .feed_icon_dir
        .parent()
        .map(|d| d.join("images"))
        .unwrap_or_default();

    // `SELECT` column indices (same layout as the view path): non-vfeed rows
    // have 30 columns; vfeed rows add `feed_title` + `favicon_avg_color`
    // after `last_published` (32 cols).
    const I_ID: usize = 0;
    const I_DATE_ENTERED: usize = 1;
    const I_TITLE: usize = 4;
    const I_UPDATED: usize = 5;
    const I_TAG_CACHE: usize = 7;
    const I_SITE_URL: usize = 9;
    const I_NOTE: usize = 10;
    const I_LINK: usize = 21;
    const I_VF_TITLE: usize = 25;
    const I_CONTENT: usize = 25; // non-vfeed
    const I_VF_CONTENT: usize = 27;

    let mut articles_atom: Vec<Value> = Vec::new();
    let mut articles_json: Vec<Value> = Vec::new();

    for r in &q.article_rows {
        let vfeed = r.columns().len() >= 32;
        let id: i32 = r.get(I_ID);
        let date_entered: Option<NaiveDateTime> = r.get(I_DATE_ENTERED);
        let title: String = r.get(I_TITLE);
        let updated: Option<NaiveDateTime> = r.get(I_UPDATED);
        let tag_cache: Option<String> = r.get(I_TAG_CACHE);
        let site_url: Option<String> = r.get(I_SITE_URL);
        let note: Option<String> = r.get(I_NOTE);
        let link: String = r.get(I_LINK);
        let row_feed_title: Option<String> = if vfeed { r.get(I_VF_TITLE) } else { None };
        let content_idx = if vfeed { I_VF_CONTENT } else { I_CONTENT };
        let content: String = r.get(content_idx);
        let author: String = r.get(content_idx + 1);

        let tags = article::get_tags(pool, owner_uid, id, Some(owner_uid), tag_cache.as_deref())
            .await;
        let enclosures = article::get_enclosures(pool, id).await;

        // `date('c', strtotime($line["updated"] ?? ''))` — `strtotime('')` is
        // `false` → the epoch.
        let updated_rfc3339 = updated
            .map(|d| d.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_string());
        let article_id = if req.orig_guid.is_empty() {
            make_article_tag_uri(id, date_entered.as_ref(), &self_url)
        } else {
            link.clone()
        };

        // `content_preview` — sanitized, truncated text preview.
        let excerpt = sanitizer::sanitize(
            &truncate_string(&strip_tags(&content), 100, "..."),
            false,
            false,
            false,
            false,
            None,
            None,
        );

        // `Sanitizer::sanitize($content, false, $owner_uid, $feed_site_url, null, $id)`
        let content_sanitized = sanitizer::sanitize(
            &content,
            false,
            strip_images,
            false,
            false,
            Some(&feed_site_url),
            None,
        );

        let source_link = site_url
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self_url.clone());
        // PHP: `$line['feed_title'] ?? $feed_title`, where
        // `$feed_title = htmlspecialchars($qfh_ret[1])`. The plain feed query
        // has no `feed_title` column, so the (already-escaped) feed title is
        // the fallback — and the Atom template variable then receives a
        // *second* escape (the JSON variant does not).
        let row_feed_title = if vfeed { row_feed_title } else { None };
        let source_title = row_feed_title.unwrap_or_else(|| feed_title.clone());

        if req.format == "atom" {
            // Atom content: sanitized + disk-cache URL rewrites; a non-empty
            // note is prepended as a styled div (raw, matching PHP).
            let mut atom_content =
                diskcache::rewrite_urls(&content_sanitized, &image_cache_dir, &self_url);
            if let Some(note) = &note {
                atom_content =
                    format!("<div style=\"{note_style}\">Article note: {note}</div>{atom_content}");
            }
            let mut a = Map::new();
            a.insert("id".into(), json!(htmlspecialchars(&article_id)));
            a.insert("link".into(), json!(htmlspecialchars(&link)));
            a.insert("title".into(), json!(htmlspecialchars(&title)));
            a.insert("excerpt".into(), json!(excerpt));
            a.insert("content".into(), json!(atom_content));
            a.insert("updated".into(), json!(updated_rfc3339));
            a.insert("author".into(), json!(htmlspecialchars(&author)));
            a.insert("source_link".into(), json!(htmlspecialchars(&source_link)));
            a.insert("source_title".into(), json!(htmlspecialchars(&source_title)));
            a.insert(
                "tags".into(),
                Value::Array(
                    tags.iter().map(|t| json!(htmlspecialchars(t))).collect(),
                ),
            );
            a.insert(
                "enclosures".into(),
                Value::Array(
                    enclosures
                        .iter()
                        .map(|e| {
                            let length = e.get("duration").and_then(Value::as_i64).filter(|&d| d != 0).unwrap_or(1);
                            let mut em = Map::new();
                            em.insert(
                                "type".into(),
                                json!(htmlspecialchars(
                                    e.get("content_type")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                )),
                            );
                            em.insert("length".into(), json!(length));
                            em.insert(
                                "href".into(),
                                json!(htmlspecialchars(
                                    e.get("content_url")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                )),
                            );
                            Value::Object(em)
                        })
                        .collect(),
                ),
            );
            articles_atom.push(Value::Object(a));
        } else {
            let mut a = Map::new();
            a.insert("id".into(), json!(link));
            a.insert("link".into(), json!(link));
            a.insert("title".into(), json!(title));
            a.insert("content".into(), json!(content_sanitized));
            a.insert("updated".into(), json!(updated_rfc3339));
            a.insert(
                "source".into(),
                json!({ "link": source_link, "title": source_title }),
            );
            if let Some(note) = &note {
                a.insert("note".into(), json!(note));
            }
            if !author.is_empty() {
                a.insert("author".into(), json!(author));
            }
            if !tags.is_empty() {
                a.insert("tags".into(), Value::Array(tags.iter().map(|t| json!(t)).collect()));
            }
            if !enclosures.is_empty() {
                a.insert(
                    "enclosures".into(),
                    Value::Array(
                        enclosures
                            .iter()
                            .map(|e| {
                                json!({
                                    "url": e.get("content_url").cloned().unwrap_or(Value::Null),
                                    "type": e.get("content_type").cloned().unwrap_or(Value::Null),
                                    "length": e.get("duration").cloned().unwrap_or(Value::Null),
                                })
                            })
                            .collect(),
                    ),
                );
            }
            articles_json.push(Value::Object(a));
        }
    }

    if req.format == "atom" {
        let mut ctx: Map<String, Value> = Map::new();
        ctx.insert("feed_title".into(), json!(feed_title));
        ctx.insert("version".into(), json!(VERSION));
        ctx.insert(
            "feed_updated".into(),
            json!(Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()),
        );
        ctx.insert("feed_url".into(), json!(htmlspecialchars(&feed_self_url)));
        ctx.insert("self_url".into(), json!(htmlspecialchars(&self_url)));
        ctx.insert("articles".into(), Value::Array(articles_atom));
        let body = atom_env()
            .get_template("feed_atom")
            .expect("atom template")
            .render(ctx)
            .expect("atom template render");
        let content_type = if req.noxml {
            "text/plain; charset=utf-8"
        } else {
            "text/xml; charset=utf-8"
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .expect("valid response")
    } else {
        let feed_json = json!({
            "title": feed_title,
            "feed_url": feed_self_url,
            "self_url": self_url,
            "articles": articles_json,
        });
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Body::from(serde_json::to_string(&feed_json).expect("json encode")))
            .expect("valid response")
    }
}

/// The minijinja environment holding `feed_atom` (parsed once).
fn atom_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template("feed_atom", include_str!("../templates/feed_atom.jinja"))
            .expect("valid atom template");
        env
    })
}

/// `Handler_Public::_make_article_tag_uri` — `tag:<host>,<Y-m-d>:/<id>`.
fn make_article_tag_uri(id: i32, date_entered: Option<&NaiveDateTime>, self_url: &str) -> String {
    // `date("Y-m-d", strtotime($timestamp))` — `strtotime('')` → epoch.
    let day = date_entered
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string());
    // `parse_url(Config::get_self_url(), PHP_URL_HOST)`
    let host = url::Url::parse(self_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    format!("tag:{host},{day}:/{id}")
}

/// `Feeds::_get_access_key($feed_id, $is_cat, $owner_uid)` — return the
/// existing key for `(owner_uid, feed_id, is_cat)` or create one
/// (`uniqid_short`).
async fn get_or_create_access_key(
    pool: &sqlx::PgPool,
    feed_id: &str,
    is_cat: bool,
    owner_uid: i64,
) -> Option<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT access_key FROM ttrss_access_keys
         WHERE owner_uid = $1 AND feed_id = $2 AND is_cat = $3",
    )
    .bind(owner_uid as i32)
    .bind(feed_id)
    .bind(is_cat)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if let Some(k) = existing {
        return Some(k);
    }
    let key = util::uniqid_short();
    let saved = sqlx::query(
        "INSERT INTO ttrss_access_keys (owner_uid, feed_id, is_cat, access_key)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(owner_uid as i32)
    .bind(feed_id)
    .bind(is_cat)
    .bind(&key)
    .execute(pool)
    .await
    .is_ok_and(|r| r.rows_affected() == 1);
    saved.then_some(key)
}

/// `ttrss_access_keys` lookup: returns the `owner_uid` for a matching
/// `(access_key, feed_id)` pair, or `None`. `owner_uid` is INT4 → decoded as
/// `i32`, widened to `i64` at the boundary.
async fn lookup_access_key(state: &AppState, req: &RssRequest<'_>) -> Option<i64> {
    let (Some(key), Some(id)) = (req.key, req.id) else {
        return None;
    };
    let uid: Option<i32> =
        sqlx::query_scalar("SELECT owner_uid FROM ttrss_access_keys WHERE access_key = $1 AND feed_id = $2")
            .bind(key)
            .bind(id)
            .fetch_optional(state.db.pool())
            .await
            .ok()
            .flatten();
    uid.map(i64::from)
}

/// `403 Forbidden` — unknown/missing access key.
fn forbidden() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
        .body(Body::empty())
        .expect("valid response")
}

/// `Unknown format: $format.` — the requested format is not `atom`/`json`.
fn unknown_format(format: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(format!("Unknown format: {format}.")))
        .expect("valid response")
}
