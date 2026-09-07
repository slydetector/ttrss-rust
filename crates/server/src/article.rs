//! Port of `classes/Article.php` (static helpers + mutation ops) and
//! `RPC::{catchupSelected, markSelected, publishSelected}` (M3: article-state
//! mutations).
//!
//! The mutation ops perform the SQL `UPDATE ttrss_user_entries` and return the
//! `UPDATE_COUNTERS` envelope:
//! `{message, labels: Article::_labels_of(ids), feeds: Article::_feeds_of(ids)}`.
//!
//! PHP calls go through `PluginHost` chains (`HOOK_ARTICLE_IMAGE`,
//! `HOOK_FORMAT_ENCLOSURES`, …) and `DiskCache` URL wrapping; the Rust server
//! has no plugin host or disk cache, so those paths are no-ops / omitted and
//! documented at the call site.

use axum::http::StatusCode;
use std::collections::HashSet;
use std::sync::OnceLock;

use kuchikiki::traits::TendrilSink;
use kuchikiki::{parse_html, NodeRef};
use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;

use crate::prefs;
use crate::sanitizer::rewrite_relative;
use crate::session::Sess;
use crate::AppState;

// `Labels::{label_to_feed_id, feed_to_label_id}` (canonical home: labels.rs).
pub use crate::labels::{feed_to_label_id, label_to_feed_id};

// `Article::*` (Article.php).
pub const CATCHUP_MODE_MARK_AS_READ: i32 = 0;
pub const CATCHUP_MODE_MARK_AS_UNREAD: i32 = 1;
pub const CATCHUP_MODE_TOGGLE: i32 = 2;
pub const ARTICLE_KIND_ALBUM: i32 = 1;
pub const ARTICLE_KIND_VIDEO: i32 = 2;
pub const ARTICLE_KIND_YOUTUBE: i32 = 3;


/// A scalar form value as its PHP string form.
fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `Handler::_param_to_int_array`: parse `name` / `name[]` into ints. A string
/// is trimmed and split on commas; any non-int rejects the whole input.
fn param_int_array(extra: &Map<String, Value>, name: &str) -> Vec<i32> {
    let raw = extra
        .get(&format!("{name}[]"))
        .or_else(|| extra.get(name));
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut parts: Vec<String> = Vec::new();
    match raw {
        Value::Array(items) => {
            for it in items {
                if let Some(s) = scalar_str(it) {
                    parts.push(s);
                }
            }
        }
        other => {
            if let Some(s) = scalar_str(other) {
                parts.push(s);
            }
        }
    }
    let mut out: Vec<i32> = Vec::new();
    let mut rejected = false;
    for p in parts {
        for tok in p.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            match t.parse::<i64>() {
                Ok(n) => out.push(n as i32),
                Err(_) => rejected = true,
            }
        }
    }
    if rejected {
        Vec::new()
    } else {
        out
    }
}

fn cmode_of(extra: &Map<String, Value>) -> i32 {
    extra
        .get("cmode")
        .and_then(scalar_str)
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

/// `$n` placeholders for a `ref_id IN (…)` clause over `ids`.
fn in_placeholders(ids: &[i32]) -> String {
    (1..=ids.len()).map(|i| format!("${i}")).collect::<Vec<_>>().join(",")
}

fn bind_ids<'a>(
    mut q: sqlx::query::Query<'a, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ids: &[i32],
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
    ids: &[i32],
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
    let _ = bind_ids(sqlx::query(&sql), ids).bind(uid).execute(pool).await;
}

/// `RPC::markArticlesById`: star / unstar / toggle the given articles.
async fn mark_articles_by_id(pool: &sqlx::PgPool, ids: &[i32], cmode: i32, uid: i64) {
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
    let _ = bind_ids(sqlx::query(&sql), ids).bind(uid).execute(pool).await;
}

/// `RPC::publishArticlesById`: publish / unpublish / toggle the given articles.
async fn publish_articles_by_id(pool: &sqlx::PgPool, ids: &[i32], cmode: i32, uid: i64) {
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
    let _ = bind_ids(sqlx::query(&sql), ids).bind(uid).execute(pool).await;
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
    id: i32,
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
/// associative arrays (PHP `as_array()`). PHP wraps cached URLs via
/// `DiskCache::get_url`; the Rust server has no disk cache, so URLs are
/// returned as stored.
pub async fn get_enclosures(pool: &sqlx::PgPool, id: i32) -> Vec<Map<String, Value>> {
    type EnclosureRow = (i32, String, String, String, String, i32, i32, i32);
    let rows: Vec<EnclosureRow> = sqlx::query_as(
        "SELECT id, content_url, content_type, title, duration, width, height, post_id
         FROM ttrss_enclosures WHERE post_id = $1",
    )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    rows.into_iter()
        .map(|(enc_id, url, ctype, title, duration, width, height, post_id)| {
            json!({
                "id": enc_id,
                "content_url": url,
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
        })
        .collect()
}

/// `Article::_get_image`: the article's representative image / video stream
/// and kind. The first `img[@src]` / `video[@poster]` / YouTube-embed
/// `iframe` in document order wins (matching the PHP XPath union);
/// otherwise the first `image/*` enclosure is used. URLs are rewritten
/// against `site_url`; a multi-image article is kind `ARTICLE_KIND_ALBUM`.
///
/// PHP additionally runs the `HOOK_ARTICLE_IMAGE` plugin chain (no-op with
/// no plugins) and wraps cached URLs via `DiskCache` (no disk cache here).
pub fn get_image(
    enclosures: &[Map<String, Value>],
    content: &str,
    site_url: &str,
) -> (String, String, i32) {
    let mut article_image = String::new();
    let mut article_stream = String::new();
    let mut article_kind = 0;

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
                if is_image {
                    if let Some(url) = enc.get("content_url").and_then(Value::as_str) {
                        article_image = url.to_string();
                        break;
                    }
                }
            }
        }

        if !article_image.is_empty() {
            match rewrite_relative(site_url, &article_image, "a", "href", "") {
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
            if let Some(abs) = rewrite_relative(site_url, &article_stream, "a", "href", "") {
                article_stream = abs;
            } else {
                article_stream.clear();
            }
        }
    }

    // (PHP DiskCache::get_url wrapping: no disk cache in the Rust server.)
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
        if let Some(el) = node.as_element() {
            if el.name.local.to_string().eq_ignore_ascii_case("source") {
                if let Some(src) = el.attributes.borrow().get("src") {
                    return Some(src.to_string());
                }
            }
        }
        let children: Vec<NodeRef> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// PHP `basename()` on a URL string (segment after the last `/`, trailing
/// slashes stripped).
fn basename(url: &str) -> String {
    url.rsplit('/').find(|s| !s.is_empty()).unwrap_or("").to_string()
}

/// `Article::_format_enclosures` result: the `formatted` / `entries` pair
/// plus the `can_inline` / `inline_text_only` flags.
#[derive(Clone, Debug, PartialEq)]
pub struct FormattedEnclosures {
    pub formatted: String,
    pub entries: Vec<Map<String, Value>>,
    pub can_inline: bool,
    pub inline_text_only: bool,
}

/// `Article::_format_enclosures`: fetch the article's enclosures and compute
/// the inline-display flags. The PHP `HOOK_FORMAT_ENCLOSURES` /
/// `HOOK_RENDER_ENCLOSURE` / `HOOK_ENCLOSURE_ENTRY` plugin chains are no-ops
/// with no plugins, so `formatted` stays empty and every enclosure lands in
/// `entries` (with `filename` = `basename(content_url)` added).
pub async fn format_enclosures(
    db: &Db,
    sess: &Session,
    id: i32,
    always_display_enclosures: bool,
    article_content: &str,
    hide_images: bool,
) -> FormattedEnclosures {
    let pool = db.pool();
    let enclosures = get_enclosures(pool, id).await;

    // $rv['can_inline'] = isset($_SESSION["uid"]) && empty($_SESSION["bw_limit"])
    //   && !Prefs::get(Prefs::STRIP_IMAGES, $_SESSION["uid"], $_SESSION["profile"] ?? null)
    //   && ($always_display_enclosures || !preg_match("/<img/i", $article_content));
    let uid = sess.get_int("uid");
    let strip_images = if let Some(uid) = uid {
        let profile = sess.get_int("profile").map(|p| p as i32);
        let rows = prefs::load_rows(db, uid, profile).await;
        prefs::resolve("STRIP_IMAGES", &rows, profile).as_bool()
    } else {
        false
    };
    let can_inline = uid.is_some()
        && php_empty(sess.get("bw_limit"))
        && !strip_images
        && (always_display_enclosures || !article_content.to_ascii_lowercase().contains("<img"));
    let inline_text_only = hide_images && can_inline;

    let mut entries: Vec<Map<String, Value>> = Vec::new();
    for enc in &enclosures {
        let mut enc = enc.clone();
        if let Some(url) = enc.get("content_url").and_then(Value::as_str) {
            enc.insert("filename".into(), Value::String(basename(url)));
        }
        entries.push(enc);
    }

    FormattedEnclosures {
        formatted: String::new(),
        entries,
        can_inline,
        inline_text_only,
    }
}

/// PHP `empty()` on a session value (absent counts as empty).
fn php_empty(v: Option<&PValue>) -> bool {
    match v {
        None | Some(PValue::Null) => true,
        Some(PValue::Bool(b)) => !b,
        Some(PValue::Int(i)) => *i == 0,
        Some(PValue::Float(f)) => *f == 0.0,
        Some(PValue::Bytes(b)) => b.is_empty(),
        Some(PValue::Array(a)) => a.is_empty(),
    }
}

/// `Article::_labels_of`: unique label ids present in the articles' `label_cache`.
pub async fn labels_of(pool: &sqlx::PgPool, ids: &[i32], uid: i64) -> Vec<i64> {
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
    let rows = bind_ids(sqlx::query(&sql), ids)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut rv: Vec<i64> = Vec::new();
    for row in &rows {
        let cache: Option<String> = row.get(0);
        if let Some(cache) = cache {
            if let Ok(v) = serde_json::from_str::<Value>(cache.trim()) {
                if let Some(arr) = v.as_array() {
                    for label in arr {
                        // Only list-form entries carry a feed id; the
                        // `{"no-labels":1}` object form is skipped.
                        if let Some(items) = label.as_array() {
                            if let Some(first) = items.first().and_then(Value::as_i64) {
                                let lid = feed_to_label_id(first);
                                if seen.insert(lid) {
                                    rv.push(lid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    rv
}

/// `Article::_feeds_of`: unique feed ids of the given articles.
pub async fn feeds_of(pool: &sqlx::PgPool, ids: &[i32], uid: i64) -> Vec<i64> {
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
    let rows = bind_ids(sqlx::query(&sql), ids)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut rv: Vec<i64> = Vec::new();
    for row in &rows {
        let feed_id: Option<i32> = row.get(0);
        if let Some(fid) = feed_id {
            if seen.insert(fid as i64) {
                rv.push(fid as i64);
            }
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
pub async fn catchup_selected(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Value {
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
pub async fn mark_selected(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let cmode = cmode_of(extra);
    if !ids.is_empty() {
        mark_articles_by_id(pool, &ids, cmode, uid).await;
    }
    let labels = labels_of(pool, &ids, uid).await;
    let feeds = feeds_of(pool, &ids, uid).await;
    update_counters(labels, feeds)
}

/// `RPC::publishSelected`.
pub async fn publish_selected(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = param_int_array(extra, "ids");
    let cmode = cmode_of(extra);
    if !ids.is_empty() {
        publish_articles_by_id(pool, &ids, cmode, uid).await;
    }
    let labels = labels_of(pool, &ids, uid).await;
    let feeds = feeds_of(pool, &ids, uid).await;
    update_counters(labels, feeds)
}

/// `RPC::mark`: star / unstar one article (`mark` is the cleaned request
/// string, cast to boolean by Postgres exactly as PHP's `marked = ?`).
/// PHP `HOOK_ARTICLES_MARK_TOGGLED` is omitted (no plugin host).
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
    json!({ "message": "UPDATE_COUNTERS" })
}

/// `RPC::publ`: publish / unpublish one article (see [`mark`]). PHP
/// `HOOK_ARTICLES_PUBLISH_TOGGLED` is omitted (no plugin host).
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
    let _ = bind_ids(sqlx::query(&sql), &ids)
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
    let _ = bind_ids(sqlx::query(&sql), &ids)
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
    let label = crate::labels::find_caption(pool, crate::util::php_int_cast(&label_id) as i32, uid).await;
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
