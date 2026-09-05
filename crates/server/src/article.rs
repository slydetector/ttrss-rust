//! Port of `classes/Article.php` mutation helpers + `RPC::{catchupSelected,
//! markSelected, publishSelected}` (M3: article-state mutations).
//!
//! Each method performs the SQL `UPDATE ttrss_user_entries` and returns the
//! `UPDATE_COUNTERS` envelope:
//! `{message, labels: Article::_labels_of(ids), feeds: Article::_feeds_of(ids)}`.
//!
//! None of the loaded plugins (`auto_assign_labels`, `note`, `share`, …) react
//! to the `HOOK_ARTICLES_{MARK,PUBLISH}_TOGGLED` hooks, so the `PluginHost`
//! `run_hooks` calls are no-ops here.

use std::collections::HashSet;

use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::session::Sess;
use crate::AppState;

// `Article::*` (Article.php).
const CATCHUP_MODE_MARK_AS_READ: i32 = 0;
const CATCHUP_MODE_MARK_AS_UNREAD: i32 = 1;
const CATCHUP_MODE_TOGGLE: i32 = 2;

// `Feeds::*` (functions.php).
const LABEL_BASE_INDEX: i64 = -1024;

fn feed_to_label_id(feed: i64) -> i64 {
    LABEL_BASE_INDEX - 1 + feed.abs()
}

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

/// `Article::_catchup_by_id`: read / unread / toggle the given articles.
async fn catchup_by_id(pool: &sqlx::PgPool, ids: &[i32], cmode: i32, uid: i64) {
    if ids.is_empty() {
        return;
    }
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

/// `Article::_labels_of`: unique label ids present in the articles' `label_cache`.
async fn labels_of(pool: &sqlx::PgPool, ids: &[i32], uid: i64) -> Vec<i64> {
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
async fn feeds_of(pool: &sqlx::PgPool, ids: &[i32], uid: i64) -> Vec<i64> {
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
        catchup_by_id(pool, &ids, cmode, uid).await;
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
