//! Port of `classes/Article.php` (static helpers + mutation ops) and
//! `RPC::{catchupSelected, markSelected, publishSelected}` (M3: article-state
//! mutations).
//!
//! The mutation ops perform the SQL `UPDATE ttrss_user_entries` and return the
//! `UPDATE_COUNTERS` envelope:
//! `{message, labels: Article::_labels_of(ids), feeds: Article::_feeds_of(ids)}`.
//!
//! PHP calls go through `PluginHost` chains (`HOOK_ARTICLE_IMAGE`,
//! `HOOK_FORMAT_ENCLOSURES`, `HOOK_ARTICLES_*_TOGGLED`, …); the port runs
//! the same chains at the same sites (session-scoped host for the RPC/API
//! routes, the owner-scoped host in `create_published_article`).

use axum::http::StatusCode;
use std::collections::HashSet;
use std::sync::OnceLock;

use crate::util::{param_int_array, scalar_str};
use kuchikiki::traits::TendrilSink;
use kuchikiki::{NodeRef, parse_html};
use serde_json::{Map, Value, json};
use sha1::Digest as _;
use sqlx::AssertSqlSafe;
use sqlx::Row;

use crate::AppState;
use crate::sanitizer::rewrite_relative;
use crate::session::Sess;

// `Labels::{label_to_feed_id, feed_to_label_id}` (canonical home: labels.rs).
pub use crate::labels::{feed_to_label_id, label_to_feed_id};

// `Article::*` (Article.php).
pub const CATCHUP_MODE_MARK_AS_READ: i32 = 0;
pub const CATCHUP_MODE_MARK_AS_UNREAD: i32 = 1;
pub const CATCHUP_MODE_TOGGLE: i32 = 2;
pub const ARTICLE_KIND_ALBUM: i32 = 1;
pub const ARTICLE_KIND_VIDEO: i32 = 2;
pub const ARTICLE_KIND_YOUTUBE: i32 = 3;

fn cmode_of(extra: &Map<String, Value>) -> i32 {
    // PHP: `(int) clean($_REQUEST["cmode"])` — clean (strip_tags + trim), then
    // PHP's truncating int cast (leading integer prefix), not a strict parse.
    extra
        .get("cmode")
        .and_then(scalar_str)
        .map(|s| crate::util::php_int_cast(&crate::util::clean_str(&s)) as i32)
        .unwrap_or(0)
}

/// `$n` placeholders for a `ref_id IN (…)` clause over `ids`.
fn in_placeholders(ids: &[i64]) -> String {
    (1..=ids.len())
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn bind_ids<'a>(
    mut q: sqlx::query::Query<'a, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ids: &[i64],
) -> sqlx::query::Query<'a, sqlx::Postgres, sqlx::postgres::PgArguments> {
    for id in ids {
        q = q.bind(*id);
    }
    q
}

/// `Article::_catchup_by_id`: read / unread / toggle the given articles for
/// `owner_uid` (the session uid when `None` / 0).
pub async fn catchup_by_id(
    pool: &sqlx::PgPool,
    ids: &[i64],
    cmode: i32,
    session_uid: i64,
    owner_uid: Option<i64>,
) {
    if ids.is_empty() {
        return;
    }
    let uid = match owner_uid {
        Some(u) if u != 0 => u,
        _ => session_uid,
    };
    let set = match cmode {
        CATCHUP_MODE_MARK_AS_UNREAD => "unread = true",
        CATCHUP_MODE_TOGGLE => "unread = NOT unread, last_read = NOW()",
        _ => "unread = false, last_read = NOW()",
    };
    let sql = format!(
        "UPDATE ttrss_user_entries SET {set} \
         WHERE ref_id IN ({}) AND owner_uid = ${}",
        in_placeholders(ids),
        ids.len() + 1
    );
    let _ = bind_ids(sqlx::query(AssertSqlSafe(sql)), ids)
        .bind(uid)
        .execute(pool)
        .await;
}

/// `RPC::markArticlesById`: star / unstar / toggle the given articles.
async fn mark_articles_by_id(
    state: &AppState,
    sess: &Sess,
    pool: &sqlx::PgPool,
    ids: &[i64],
    cmode: i32,
    uid: i64,
) {
    if ids.is_empty() {
        return;
    }
    let set = match cmode {
        CATCHUP_MODE_MARK_AS_READ => "marked = false, last_marked = NOW()",
        CATCHUP_MODE_MARK_AS_UNREAD => "marked = true, last_marked = NOW()",
        _ => "marked = NOT marked, last_marked = NOW()",
    };
    let sql = format!(
        "UPDATE ttrss_user_entries SET {set} \
         WHERE ref_id IN ({}) AND owner_uid = ${}",
        in_placeholders(ids),
        ids.len() + 1
    );
    let _ = bind_ids(sqlx::query(AssertSqlSafe(sql)), ids)
        .bind(uid)
        .execute(pool)
        .await;
    // `run_hooks(HOOK_ARTICLES_MARK_TOGGLED, $ids)` (RPC.php:268).
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    pctx.run_hooks(
        crate::pluginhost::HOOK_ARTICLES_MARK_TOGGLED,
        |plugin, host| {
            let ids = ids.to_vec();
            Box::pin(async move { plugin.hook_articles_mark_toggled(host, &ids).await })
        },
    )
    .await;
}

/// `RPC::publishArticlesById`: publish / unpublish / toggle the given
/// articles.
async fn publish_articles_by_id(
    state: &AppState,
    sess: &Sess,
    pool: &sqlx::PgPool,
    ids: &[i64],
    cmode: i32,
    uid: i64,
) {
    if ids.is_empty() {
        return;
    }
    let published_val = match cmode {
        CATCHUP_MODE_MARK_AS_READ => "false",
        CATCHUP_MODE_MARK_AS_UNREAD => "true",
        _ => "NOT published",
    };
    let sql = format!(
        "UPDATE ttrss_user_entries SET published = {}, last_published = NOW() \
         WHERE ref_id IN ({}) AND owner_uid = ${}",
        published_val,
        in_placeholders(ids),
        ids.len() + 1
    );
    let _ = bind_ids(sqlx::query(AssertSqlSafe(sql)), ids)
        .bind(uid)
        .execute(pool)
        .await;
    // `run_hooks(HOOK_ARTICLES_PUBLISH_TOGGLED, $ids)` (RPC.php:296).
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    pctx.run_hooks(
        crate::pluginhost::HOOK_ARTICLES_PUBLISH_TOGGLED,
        |plugin, host| {
            let ids = ids.to_vec();
            Box::pin(async move { plugin.hook_articles_publish_toggled(host, &ids).await })
        },
    )
    .await;
}

/// `Article::_get_tags`: the tags of article `id` for `owner_uid` (the
/// session uid when `None`/0). With a non-empty `tag_cache` it is simply
/// split on commas; otherwise the tags come from `ttrss_tags` (hard path)
/// and the cache is updated.
pub async fn get_tags(
    pool: &sqlx::PgPool,
    session_uid: i64,
    id: i32,
    owner_uid: Option<i64>,
    tag_cache: Option<&str>,
) -> Vec<String> {
    let owner = match owner_uid {
        Some(u) if u != 0 => u as i32,
        _ => session_uid as i32,
    };

    let mut cache = tag_cache.map(str::to_string);
    if cache.as_deref().is_none_or(str::is_empty) {
        let c: Option<String> = sqlx::query_scalar(
            "SELECT tag_cache FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
        )
        .bind(id)
        .bind(owner)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        if c.as_deref().is_some_and(|c| !c.is_empty()) {
            cache = c;
        }
    }

    if let Some(cache) = cache.as_deref().filter(|c| !c.is_empty()) {
        return cache.split(',').map(str::to_string).collect();
    }

    // The hard way: distinct tag names for this user entry's int_id.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT tag_name FROM ttrss_tags
         WHERE post_int_id = (SELECT int_id FROM ttrss_user_entries
             WHERE ref_id = $1 AND owner_uid = $2 LIMIT 1)
         ORDER BY tag_name",
    )
    .bind(id)
    .bind(owner)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // Update the cache.
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET tag_cache = $1 WHERE ref_id = $2 AND owner_uid = $3",
    )
    .bind(rows.join(","))
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await;

    rows
}

/// `Article::_get_labels`: the labels of article `id` for `owner_uid` (the
/// session uid when `None`/0), as `[feed_id, caption, fg_color, bg_color]`
/// entries (JSON values, preserving the cached types). An empty label set —
/// including the `{"no-labels":1}` cache marker — is `[]`.
///
/// On the hard path (no usable cache) the result is written back via
/// `Labels::update_cache`.
pub async fn get_labels(
    pool: &sqlx::PgPool,
    session_uid: i64,
    id: i64,
    owner_uid: Option<i64>,
) -> Vec<Value> {
    let owner = match owner_uid {
        Some(u) if u != 0 => u as i32,
        _ => session_uid as i32,
    };

    // The typed form of `labels::get_labels_json` (the same
    // `Article::_get_labels` semantics); a decodable non-array cache value
    // is treated as no labels (PHP would return the decoded scalar as-is).
    let cached = crate::labels::get_labels_json(pool, owner as i64, id).await;
    match serde_json::from_str::<Value>(&cached) {
        Ok(Value::Array(arr)) => arr,
        _ => Vec::new(),
    }
}

/// `Article::getmetadatabyid`: `{link, title}` of article `id` owned by `uid`,
/// or `[]` if none.
pub async fn metadata_by_id(pool: &sqlx::PgPool, uid: i64, id: i32) -> Value {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT e.link, e.title FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         WHERE e.id = $1 AND ue.owner_uid = $2 LIMIT 1",
    )
    .bind(id)
    .bind(uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    match row {
        Some((link, title)) => json!({ "link": link, "title": title }),
        None => json!([]),
    }
}

/// `Article::printArticleTags`: `{"id": <id>, "tags": [tag names]}`.
pub async fn print_article_tags(pool: &sqlx::PgPool, session_uid: i64, id: i32) -> Value {
    let tags = get_tags(pool, session_uid, id, None, None).await;
    json!({ "id": id, "tags": tags })
}

/// `Article::_purge_orphans`: delete entries no user references.
pub async fn purge_orphans(pool: &sqlx::PgPool) {
    let _ = sqlx::query(
        "DELETE FROM ttrss_entries WHERE NOT EXISTS (
            SELECT ref_id FROM ttrss_user_entries WHERE ref_id = id)",
    )
    .execute(pool)
    .await;
}

/// `Article::_get_enclosures`: the article's enclosure rows as
/// associative arrays (PHP `as_array()`). A locally cached media copy
/// rewrites `content_url` to the `op=cached` serve path (PHP's
/// `DiskCache::instance("images")` wrap).
pub async fn get_enclosures(
    pool: &sqlx::PgPool,
    id: i32,
    cache_dir: &std::path::Path,
    self_url: &str,
) -> Vec<Map<String, Value>> {
    type EnclosureRow = (i32, String, String, String, String, i32, i32, i32);
    let rows: Vec<EnclosureRow> = sqlx::query_as(
        "SELECT id, content_url, content_type, title, duration, width, height, post_id
         FROM ttrss_enclosures WHERE post_id = $1",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let cache = crate::diskcache::DiskCache::new(cache_dir, "images", self_url);
    rows.into_iter()
        .map(
            |(enc_id, url, ctype, title, duration, width, height, post_id)| {
                // PHP: `$cache->exists(sha1($content_url))` → `get_url(…)`.
                let key = crate::util::sha1_hex(&url);
                let content_url = if cache.exists(&key) {
                    cache.get_url(&key)
                } else {
                    url
                };
                json!({
                    "id": enc_id,
                    "content_url": content_url,
                    "content_type": ctype,
                    "title": title,
                    "duration": duration,
                    "width": width,
                    "height": height,
                    "post_id": post_id,
                })
                .as_object()
                .unwrap()
                .clone()
            },
        )
        .collect()
}

/// `Article::_get_image`: the article's representative image / video stream
/// and kind. The first `img[@src]` / `video[@poster]` / YouTube-embed
/// `iframe` in document order wins (matching the PHP XPath union);
/// otherwise the first `image/*` enclosure is used. URLs are rewritten
/// against `site_url`; a multi-image article is kind `ARTICLE_KIND_ALBUM`.
///
/// M7.22 item 4: the `HOOK_ARTICLE_IMAGE` chain runs first (PHP
/// `Article.php:571`) — each plugin returns `[image, stream, content]`;
/// the chain stops at the first non-empty image ("first hard match"), and
/// the plugins receive the original arguments (PHP's fixed `$args`). The
/// fallback parse uses the last plugin's content (nsfw returns `"nsfw"` on
/// a match — the parse is skipped anyway once an image is set).
pub async fn get_image(
    host: &mut crate::pluginhost::host::HostCtx,
    enclosures: &[Map<String, Value>],
    content: &str,
    site_url: &str,
    article: &Value,
) -> (String, String, i32) {
    let mut article_image = String::new();
    let mut article_stream = String::new();
    let mut article_kind = 0;
    let mut content = content.to_string();

    {
        let enc_val: Value = enclosures.iter().cloned().map(Value::Object).collect();
        // (enclosures, content, site_url, article, last plugin result)
        let mut state = (
            enc_val,
            content.clone(),
            site_url.to_string(),
            article.clone(),
            Value::Null,
        );
        host.chain_hooks_callback(
            crate::pluginhost::HOOK_ARTICLE_IMAGE,
            &mut state,
            |plugin, host, st| {
                Box::pin(async move {
                    let r = plugin
                        .hook_article_image(host, &st.0, &st.1, &st.2, &st.3)
                        .await;
                    st.4 = r.clone();
                    r
                })
            },
            // PHP: `return !empty($article_image)` — first hard match.
            |r, _| {
                r.get(0)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            },
        )
        .await;
        if let Some(triple) = state.4.as_array() {
            article_image = triple
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            article_stream = triple
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(c) = triple.get(2).and_then(Value::as_str) {
                content = c.to_string();
            }
        }
    }

    if article_image.is_empty() && article_stream.is_empty() {
        // PHP: `mb_substr($content, 0, 131070)` before `loadHTML`.
        let content: String = content.chars().take(131_070).collect();
        let doc = parse_html().one(content);
        let elems = collect_image_candidates(&doc);

        for e in &elems {
            let Some(el) = e.as_element() else {
                continue;
            };
            let tag: String = el.name.local.to_string().to_ascii_lowercase();
            let attrs = el.attributes.borrow();
            if tag == "iframe" {
                let Some(src) = attrs.get("src") else {
                    continue;
                };
                let Some(m) = youtube_embed_re().captures(src) else {
                    continue;
                };
                let vid = &m[1];
                article_image = format!("https://img.youtube.com/vi/{vid}/hqdefault.jpg");
                article_stream = format!("https://youtu.be/{vid}");
                article_kind = ARTICLE_KIND_YOUTUBE;
                break;
            } else if tag == "video" {
                article_image = attrs.get("poster").unwrap_or_default().to_string();
                if let Some(src) = first_source_src(e) {
                    article_stream = src;
                    article_kind = ARTICLE_KIND_VIDEO;
                }
                break;
            } else if tag == "img" {
                let src = attrs.get("src").unwrap_or_default();
                if !src.starts_with("data:") {
                    article_image = src.to_string();
                }
                break;
            }
        }

        if article_image.is_empty() {
            for enc in enclosures {
                let is_image = enc
                    .get("content_type")
                    .and_then(Value::as_str)
                    .is_some_and(|ct| ct.contains("image/"));
                if is_image && let Some(url) = enc.get("content_url").and_then(Value::as_str) {
                    article_image = url.to_string();
                    break;
                }
            }
        }

        if !article_image.is_empty() {
            // PHP `_get_image` uses the rewrite defaults (owner
            // element/attribute empty) — no `EXTRA_HREF_SCHEMES` passthrough.
            match rewrite_relative(site_url, &article_image, "", "", "") {
                Some(abs) => {
                    article_image = abs;
                    if article_kind == 0 && (enclosures.len() > 1 || elems.len() > 1) {
                        article_kind = ARTICLE_KIND_ALBUM;
                    }
                }
                // PHP: rewrite failure returns false -> article_image false.
                None => article_image.clear(),
            }
        }

        if !article_stream.is_empty() {
            if let Some(abs) = rewrite_relative(site_url, &article_stream, "", "", "") {
                article_stream = abs;
            } else {
                article_stream.clear();
            }
        }
    }

    // `DiskCache::instance("images")` — a locally cached copy rewrites the
    // URL to the `op=cached` serve path (the feed-update
    // `cache_media`/`cache_enclosures` populate the dir).
    let cache = crate::diskcache::DiskCache::new(&host.cache_dir, "images", &host.self_url);
    if !article_image.is_empty() {
        let key = crate::util::sha1_hex(&article_image);
        if cache.exists(&key) {
            article_image = cache.get_url(&key);
        }
    }
    if !article_stream.is_empty() {
        let key = crate::util::sha1_hex(&article_stream);
        if cache.exists(&key) {
            article_stream = cache.get_url(&key);
        }
    }

    (article_image, article_stream, article_kind)
}

/// `preg_match("/\\/embed\\/([\\w-]+)/")` from `Article::_get_image`.
fn youtube_embed_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"/embed/([\w-]+)").unwrap())
}

/// The XPath union `(//img[@src]|//video[@poster]|//iframe[contains(@src,
/// "youtube.com/embed/")])`, in document order.
fn collect_image_candidates(root: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    // Iterative DFS (children pushed in reverse) = document order, deep-nesting
    // safe.
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element() {
            let tag: String = el.name.local.to_string().to_ascii_lowercase();
            let attrs = el.attributes.borrow();
            let match_ = match tag.as_str() {
                "img" => attrs.get("src").is_some(),
                "video" => attrs.get("poster").is_some(),
                "iframe" => attrs
                    .get("src")
                    .is_some_and(|s| s.contains("youtube.com/embed/")),
                _ => false,
            };
            if match_ {
                out.push(node.clone());
            }
        }
        let children: Vec<NodeRef> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

/// `$tmpxpath->query("//source[@src]", $e)->item(0)` — the first `source`
/// descendant (document order) carrying a `src` attribute. Iterative: the
/// parsed tree can be arbitrarily deep.
fn first_source_src(root: &NodeRef) -> Option<String> {
    // Iterative DFS = document order.
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element()
            && el.name.local.to_string().eq_ignore_ascii_case("source")
            && let Some(src) = el.attributes.borrow().get("src")
        {
            return Some(src.to_string());
        }
        let children: Vec<NodeRef> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// `Html2Text::convert` (approximated): the visible text of an HTML fragment,
/// with `<script>`/`<style>`/`<head>` subtrees dropped and whitespace
/// collapsed. Used for the `tsvector_combined` full-text column.
fn html_to_text(content: &str) -> String {
    let doc = parse_html().one(content);
    let mut out = String::new();
    let mut stack = vec![doc];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element() {
            let tag = el.name.local.to_string().to_ascii_lowercase();
            if matches!(
                tag.as_str(),
                "script" | "style" | "noscript" | "template" | "head"
            ) {
                continue;
            }
        }
        if let Some(text) = node.as_text() {
            out.push_str(text.borrow().as_str());
        }
        let children: Vec<NodeRef> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `INSERT INTO ttrss_user_entries (…published=true, unread=false,
/// last_published=NOW(), last_read=NOW())` for a shared article.
async fn insert_shared_user_entry(tx: &mut sqlx::PgConnection, ref_id: i32, owner_uid: i64) {
    let _ = sqlx::query(
        "INSERT INTO ttrss_user_entries (ref_id, uuid, feed_id, orig_feed_id, owner_uid, \
         published, tag_cache, label_cache, last_read, note, unread, last_published) \
         VALUES ($1, '', NULL, NULL, $2, true, '', '', NOW(), '', false, NOW())",
    )
    .bind(ref_id)
    .bind(owner_uid)
    .execute(tx)
    .await;
}

/// `Article::_create_published_article` — create or update a shared article
/// for `$owner_uid` and mark it published. Returns true on success (requires a
/// valid URL and a non-empty title-or-url).
pub async fn create_published_article(
    pool: &sqlx::PgPool,
    title: &str,
    url: &str,
    content: &str,
    labels_str: &str,
    owner_uid: i64,
    mut pctx: crate::pluginhost::host::HostCtx,
) -> bool {
    // `sha1("ttshared:" . $url . $owner_uid)` → `SHA1:<hex>` (owner_uid in the
    // GUID prevents a global clash).
    let guid = format!(
        "SHA1:{:x}",
        sha1::Sha1::digest(format!("ttshared:{}{}", url, owner_uid).as_bytes())
    );

    // `if (!$content)` — HOOK_GET_FULL_TEXT (Article.php:44): a fresh
    // owner-scoped host; the first non-empty result becomes the content.
    let mut content = content.to_string();
    if content.is_empty() {
        let url_owned = url.to_string();
        pctx.run_hooks_callback::<String, _, _>(
            crate::pluginhost::HOOK_GET_FULL_TEXT,
            |plugin, host_ctx| {
                let u = url_owned.clone();
                Box::pin(async move { plugin.hook_get_full_text(host_ctx, &u).await })
            },
            |result, _| {
                if !result.is_empty() {
                    content = result.clone();
                }
                !result.is_empty()
            },
        )
        .await;
    }
    let content_hash = format!("{:x}", sha1::Sha1::digest(content.as_bytes()));
    let labels: Vec<String> = if labels_str.is_empty() {
        Vec::new()
    } else {
        labels_str.split(',').map(|s| s.to_string()).collect()
    };

    let mut title = title.to_string();
    if title.is_empty() {
        if url.is_empty() {
            return false;
        }
        title = url.to_string();
    }
    if !crate::urlhelper::filter_validate_url(url) {
        return false;
    }

    let mut tx = pool.begin().await.expect("begin tx");
    // Only check for our user's data (others may have shared it differently).
    let existing: Option<i32> = sqlx::query_scalar(
        "SELECT e.id FROM ttrss_entries e, ttrss_user_entries ue \
         WHERE e.guid = $1 AND ue.ref_id = e.id AND ue.owner_uid = $2 LIMIT 1",
    )
    .bind(&guid)
    .bind(owner_uid)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();

    let mut rc = false;
    if let Some(ref_id) = existing {
        let _ =
            sqlx::query("UPDATE ttrss_entries SET content = $1, content_hash = $2 WHERE id = $3")
                .bind(&content)
                .bind(&content_hash)
                .bind(ref_id)
                .execute(&mut *tx)
                .await;
        let _ = sqlx::query(
            "UPDATE ttrss_entries SET tsvector_combined = to_tsvector($1) WHERE id = $2",
        )
        .bind(html_to_text(&content))
        .bind(ref_id)
        .execute(&mut *tx)
        .await;
        let int_id: Option<i32> = sqlx::query_scalar(
            "SELECT int_id FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2 LIMIT 1",
        )
        .bind(ref_id)
        .bind(owner_uid)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
        if let Some(int_id) = int_id {
            let _ = sqlx::query(
                "UPDATE ttrss_user_entries SET published = true, last_published = NOW() \
                 WHERE int_id = $1 AND owner_uid = $2",
            )
            .bind(int_id)
            .bind(owner_uid)
            .execute(&mut *tx)
            .await;
        } else {
            insert_shared_user_entry(&mut tx, ref_id, owner_uid).await;
        }
        // `run_hooks(HOOK_ARTICLES_PUBLISH_TOGGLED, [$ref_id])`
        // (Article.php:105/116).
        pctx.run_hooks(
            crate::pluginhost::HOOK_ARTICLES_PUBLISH_TOGGLED,
            |plugin, host| {
                Box::pin(async move {
                    plugin
                        .hook_articles_publish_toggled(host, &[ref_id as i64])
                        .await
                })
            },
        )
        .await;
        for label in &labels {
            crate::labels::add_article(pool, ref_id as i64, label.trim(), owner_uid).await;
        }
        rc = true;
    } else {
        let ref_id: Option<i32> = sqlx::query_scalar(
            "INSERT INTO ttrss_entries \
             (title, guid, link, updated, content, content_hash, date_entered, date_updated) \
             VALUES ($1, $2, $3, NOW(), $4, $5, NOW(), NOW()) RETURNING id",
        )
        .bind(&title)
        .bind(&guid)
        .bind(url)
        .bind(&content)
        .bind(&content_hash)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
        if let Some(ref_id) = ref_id {
            let _ = sqlx::query(
                "UPDATE ttrss_entries SET tsvector_combined = to_tsvector($1) WHERE id = $2",
            )
            .bind(html_to_text(&content))
            .bind(ref_id)
            .execute(&mut *tx)
            .await;
            insert_shared_user_entry(&mut tx, ref_id, owner_uid).await;
            // `run_hooks(HOOK_ARTICLES_PUBLISH_TOGGLED, [$ref_id])`
            // (Article.php:116).
            pctx.run_hooks(
                crate::pluginhost::HOOK_ARTICLES_PUBLISH_TOGGLED,
                |plugin, host| {
                    Box::pin(async move {
                        plugin
                            .hook_articles_publish_toggled(host, &[ref_id as i64])
                            .await
                    })
                },
            )
            .await;
            for label in &labels {
                crate::labels::add_article(pool, ref_id as i64, label.trim(), owner_uid).await;
            }
            rc = true;
        }
    }

    let _ = tx.commit().await;
    rc
}

/// `Article::_labels_of`: unique label ids present in the articles' `label_cache`.
pub async fn labels_of(pool: &sqlx::PgPool, ids: &[i64], uid: i64) -> Vec<i64> {
    if ids.is_empty() {
        return Vec::new();
    }
    let sql = format!(
        "SELECT ue.label_cache FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         WHERE e.id IN ({}) AND ue.owner_uid = ${}",
        in_placeholders(ids),
        ids.len() + 1
    );
    let rows = bind_ids(sqlx::query(AssertSqlSafe(sql)), ids)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut rv: Vec<i64> = Vec::new();
    for row in &rows {
        let cache: Option<String> = row.get(0);
        if let Some(cache) = cache
            && let Ok(v) = serde_json::from_str::<Value>(cache.trim())
            && let Some(arr) = v.as_array()
        {
            for label in arr {
                // Only list-form entries carry a feed id; the
                // `{"no-labels":1}` object form is skipped.
                if let Some(items) = label.as_array()
                    && let Some(first) = items.first().and_then(Value::as_i64)
                {
                    let lid = feed_to_label_id(first);
                    if seen.insert(lid) {
                        rv.push(lid);
                    }
                }
            }
        }
    }
    rv
}

/// `Article::_feeds_of`: unique feed ids of the given articles.
pub async fn feeds_of(pool: &sqlx::PgPool, ids: &[i64], uid: i64) -> Vec<i64> {
    if ids.is_empty() {
        return Vec::new();
    }
    let sql = format!(
        "SELECT ue.feed_id FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         WHERE e.id IN ({}) AND ue.owner_uid = ${}",
        in_placeholders(ids),
        ids.len() + 1
    );
    let rows = bind_ids(sqlx::query(AssertSqlSafe(sql)), ids)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut rv: Vec<i64> = Vec::new();
    for row in &rows {
        let feed_id: Option<i32> = row.get(0);
        if let Some(fid) = feed_id
            && seen.insert(fid as i64)
        {
            rv.push(fid as i64);
        }
    }
    rv
}

fn update_counters(labels: Vec<i64>, feeds: Vec<i64>) -> Value {
    json!({
        "message": "UPDATE_COUNTERS",
        "labels": labels,
        "feeds": feeds,
    })
}

/// `RPC::catchupSelected`.
pub async fn catchup_selected(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let cmode = cmode_of(extra);
    if !ids.is_empty() {
        catchup_by_id(pool, &ids, cmode, uid, None).await;
    }
    let labels = labels_of(pool, &ids, uid).await;
    let feeds = feeds_of(pool, &ids, uid).await;
    update_counters(labels, feeds)
}

/// `RPC::markSelected`.
pub async fn mark_selected(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let cmode = cmode_of(extra);
    if !ids.is_empty() {
        mark_articles_by_id(state, sess, pool, &ids, cmode, uid).await;
    }
    let labels = labels_of(pool, &ids, uid).await;
    let feeds = feeds_of(pool, &ids, uid).await;
    update_counters(labels, feeds)
}

/// `RPC::publishSelected`.
pub async fn publish_selected(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let cmode = cmode_of(extra);
    if !ids.is_empty() {
        publish_articles_by_id(state, sess, pool, &ids, cmode, uid).await;
    }
    let labels = labels_of(pool, &ids, uid).await;
    let feeds = feeds_of(pool, &ids, uid).await;
    update_counters(labels, feeds)
}

/// `RPC::mark`: star / unstar one article (`mark` is the cleaned request
/// string, cast to boolean by Postgres exactly as PHP's `marked = ?`).
pub async fn mark(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let mark = cleaned_str(extra, "mark");
    let id = cleaned_str(extra, "id");
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET marked = $1::boolean, last_marked = NOW()
         WHERE ref_id = $2::int AND owner_uid = $3",
    )
    .bind(&mark)
    .bind(&id)
    .bind(uid)
    .execute(state.db.pool())
    .await;
    // `run_hooks(HOOK_ARTICLES_MARK_TOGGLED, [$id])` (RPC.php:72).
    if let Ok(id_i) = id.parse::<i32>() {
        let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
        pctx.run_hooks(
            crate::pluginhost::HOOK_ARTICLES_MARK_TOGGLED,
            |plugin, host| {
                Box::pin(async move {
                    plugin
                        .hook_articles_mark_toggled(host, &[id_i as i64])
                        .await
                })
            },
        )
        .await;
    }
    json!({ "message": "UPDATE_COUNTERS" })
}

/// `RPC::publ`: publish / unpublish one article (see [`mark`]).
pub async fn publ(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pub_ = cleaned_str(extra, "pub");
    let id = cleaned_str(extra, "id");
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET published = $1::boolean, last_published = NOW()
         WHERE ref_id = $2::int AND owner_uid = $3",
    )
    .bind(&pub_)
    .bind(&id)
    .bind(uid)
    .execute(state.db.pool())
    .await;
    // `run_hooks(HOOK_ARTICLES_PUBLISH_TOGGLED, [$id])` (RPC.php:100).
    if let Ok(id_i) = id.parse::<i32>() {
        let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
        pctx.run_hooks(
            crate::pluginhost::HOOK_ARTICLES_PUBLISH_TOGGLED,
            |plugin, host| {
                Box::pin(async move {
                    plugin
                        .hook_articles_publish_toggled(host, &[id_i as i64])
                        .await
                })
            },
        )
        .await;
    }
    json!({ "message": "UPDATE_COUNTERS" })
}

/// `RPC::delete`: remove the given articles from the session user's
/// subscriptions. An empty id list is a no-op with no output (PHP returns
/// before printing); [`Value::Null`] models that.
pub async fn delete(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let ids = param_int_array(extra, "ids");
    if ids.is_empty() {
        return Value::Null; // PHP: `if (!$ids) return;` (no output)
    }
    let uid = sess.data().uid().unwrap_or(0);
    let sql = format!(
        "DELETE FROM ttrss_user_entries WHERE ref_id IN ({}) AND owner_uid = ${}",
        in_placeholders(&ids),
        ids.len() + 1
    );
    let _ = bind_ids(sqlx::query(AssertSqlSafe(sql)), &ids)
        .bind(uid)
        .execute(state.db.pool())
        .await;
    json!({ "message": "UPDATE_COUNTERS" })
}

/// `Article::setScore`: set the session user's score for the given
/// articles. `{"id": [...], "score": n}`; an empty id list is a no-op with
/// no output (PHP returns before printing) — [`Value::Null`] models that.
pub async fn set_score(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let ids = param_int_array(extra, "ids");
    if ids.is_empty() {
        return Value::Null; // PHP: `if (!$ids) return;` (no output)
    }
    let uid = sess.data().uid().unwrap_or(0);
    let score = crate::util::clean_int(extra.get("score")) as i32;
    let sql = format!(
        "UPDATE ttrss_user_entries SET score = ${} WHERE ref_id IN ({}) AND owner_uid = ${}",
        ids.len() + 1,
        in_placeholders(&ids),
        ids.len() + 2
    );
    let _ = bind_ids(sqlx::query(AssertSqlSafe(sql)), &ids)
        .bind(score)
        .bind(uid)
        .execute(state.db.pool())
        .await;
    json!({ "id": ids, "score": score })
}

/// `Article::setArticleTags`: replace article `id`'s tags for the session
/// user with `tags_str` (split on `,[\r\n]+`, normalized via
/// `util::normalize_categories`), in a transaction; the `tag_cache` column
/// is updated to the comma-joined tag list. Returns `"id" => (int)$id, "tags"
/// => _get_tags($id)` re-read from the database (the original `tags_str`
/// is sometimes JSON-encoded as a hash — PHP re-reads for that reason).
pub async fn set_article_tags(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0) as i32;
    let id = extra
        .get("id")
        .and_then(scalar_str)
        .map(|s| crate::util::php_int_cast(&s))
        .unwrap_or(0) as i32;
    // PHP: `preg_split('/[,\r\n]+/', clean($_REQUEST['tags_str'] ?? ''), -1,
    // PREG_SPLIT_NO_EMPTY)`.
    let raw_tags: Vec<String> = extra
        .get("tags_str")
        .and_then(scalar_str)
        .map(|s| crate::util::clean_str(&s))
        .unwrap_or_default()
        .split([',', '\r', '\n'])
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let tags = crate::util::normalize_categories(&raw_tags);

    let pool = state.db.pool();
    let int_id: Option<i32> = sqlx::query_scalar(
        "SELECT int_id FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2 LIMIT 1",
    )
    .bind(id)
    .bind(uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if let (Some(int_id), Ok(mut tx)) = (int_id, pool.begin().await) {
        let _ = sqlx::query("DELETE FROM ttrss_tags WHERE post_int_id = $1 AND owner_uid = $2")
            .bind(int_id)
            .bind(uid)
            .execute(&mut *tx)
            .await;
        let mut tags_to_cache: Vec<String> = Vec::new();
        for tag in &tags {
            let exists: Option<i32> = sqlx::query_scalar(
                "SELECT post_int_id FROM ttrss_tags
                 WHERE post_int_id = $1 AND owner_uid = $2 AND tag_name = $3",
            )
            .bind(int_id)
            .bind(uid)
            .bind(tag)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
            if exists.is_none() {
                let _ = sqlx::query(
                    "INSERT INTO ttrss_tags (post_int_id, owner_uid, tag_name)
                     VALUES ($1, $2, $3)",
                )
                .bind(int_id)
                .bind(uid)
                .bind(tag)
                .execute(&mut *tx)
                .await;
            }
            tags_to_cache.push(tag.clone());
        }
        // update the tag cache
        let _ = sqlx::query(
            "UPDATE ttrss_user_entries SET tag_cache = $1 WHERE ref_id = $2 AND owner_uid = $3",
        )
        .bind(tags_to_cache.join(","))
        .bind(id)
        .bind(uid)
        .execute(&mut *tx)
        .await;
        let _ = tx.commit().await;
    }

    let tags = get_tags(pool, uid as i64, id, None, None).await;
    json!({ "id": id, "tags": tags })
}

/// `Article::completeTags`: tag-name completion for the session user
/// (case-sensitive prefix `LIKE`, first 10, ordered by name).
pub async fn complete_tags(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0) as i32;
    let search = cleaned_str(extra, "search");
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT tag_name FROM ttrss_tags
         WHERE owner_uid = $1 AND tag_name LIKE $2 ORDER BY tag_name
         LIMIT 10",
    )
    .bind(uid)
    .bind(format!("{search}%"))
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    Value::Array(rows.into_iter().map(Value::from).collect())
}

/// `Article::assigntolabel` / `Article::removefromlabel` (PHP
/// `_label_ops($assign)`): attach/detach the label `lid` from each id, and
/// reply with each id's fresh label list. A `lid` that resolves to no
/// caption is a no-op (PHP `if ($label)`).
pub async fn label_ops(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
    assign: bool,
) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let label_id = cleaned_str(extra, "lid");
    // PHP passes the cleaned string to `Labels::find_caption` (typed `int`
    // — PHP's string→int coercion is the leading-integer prefix).
    let label =
        crate::labels::find_caption(pool, crate::util::php_int_cast(&label_id) as i32, uid).await;
    let mut labels_for: Vec<Value> = Vec::new();
    if !label.is_empty() {
        for &id in &ids {
            if assign {
                crate::labels::add_article(pool, id, &label, uid).await;
            } else {
                crate::labels::remove_article(pool, id, &label, uid).await;
            }
            labels_for.push(json!({
                "id": id,
                "labels": get_labels(pool, uid, id, None).await,
            }));
        }
    }
    json!({ "labels-for": labels_for, "message": "UPDATE_COUNTERS" })
}

/// `Article::redirect`: 302 to the article's URL (validated with
/// `UrlHelper::validate`), or 404 + message when the article is not found or
/// the URL fails validation. Returns `(status, Location, body)`.
pub async fn redirect(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> (StatusCode, Option<String>, String) {
    let uid = sess.data().uid().unwrap_or(0);
    let id = crate::util::clean_int(extra.get("id")) as i32;
    let link: Option<String> = sqlx::query_scalar(
        "SELECT e.link FROM ttrss_entries e
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id
         WHERE e.id = $1 AND ue.owner_uid = $2",
    )
    .bind(id)
    .bind(uid)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    match link {
        None => (StatusCode::NOT_FOUND, None, "Article not found.".into()),
        Some(link) => match crate::sanitizer::validate(&link) {
            Some(url) => (StatusCode::FOUND, Some(url), String::new()),
            None => (
                StatusCode::NOT_FOUND,
                None,
                "Article URL did not pass validation.  No redirection performed.".into(),
            ),
        },
    }
}

/// `clean($_REQUEST[name])` for the string form: `trim(strip_tags(v))`.
fn cleaned_str(extra: &Map<String, Value>, name: &str) -> String {
    let Some(s) = extra.get(name).and_then(scalar_str) else {
        return String::new();
    };
    crate::util::clean_str(&s)
}
