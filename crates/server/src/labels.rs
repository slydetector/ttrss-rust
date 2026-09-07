//! Port of `classes/Labels.php` — the label CRUD + cache helpers, and the
//! `label_to_feed_id` / `feed_to_label_id` id-space mapping (the canonical
//! home for `LABEL_BASE_INDEX`).
//!
//! PHP `Labels::create` / `Labels::remove` run inside a transaction (unless
//! one is already in progress); this port always uses a transaction, matching
//! the no-ambient-transaction call paths in the server.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::db::Db;

/// `LABEL_BASE_INDEX` (functions.php) — the base of the label id space.
pub const LABEL_BASE_INDEX: i64 = -1024;

/// `Labels::label_to_feed_id`: a label id as its negative feed-space id.
pub fn label_to_feed_id(label: i64) -> i64 {
    LABEL_BASE_INDEX - 1 - label.abs()
}

/// `Labels::feed_to_label_id`: a (label-space) feed id back to a label id.
pub fn feed_to_label_id(feed: i64) -> i64 {
    LABEL_BASE_INDEX - 1 + feed.abs()
}

/// `Labels::find_id` — the label id for a (case-insensitive) caption and
/// owner, or 0 when it does not exist.
pub async fn find_id(pool: &sqlx::PgPool, caption: &str, owner_uid: i64) -> i32 {
    let row: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_labels2 WHERE LOWER(caption) = LOWER($1)
         AND owner_uid = $2 LIMIT 1",
    )
    .bind(caption)
    .bind(owner_uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    row.unwrap_or(0)
}

/// `Labels::find_caption` — the caption for a label id and owner, or `""`.
pub async fn find_caption(pool: &sqlx::PgPool, label: i32, owner_uid: i64) -> String {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT caption FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2 LIMIT 1",
    )
    .bind(label)
    .bind(owner_uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    row.unwrap_or_default()
}

/// `Labels::get_all` — `{id, fg_color, bg_color, caption}` rows for the
/// owner, ordered by caption.
pub async fn get_all(pool: &sqlx::PgPool, owner_uid: i64) -> Vec<Map<String, Value>> {
    let rows = sqlx::query(
        "SELECT id, fg_color, bg_color, caption FROM ttrss_labels2
         WHERE owner_uid = $1 ORDER BY caption",
    )
    .bind(owner_uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows
        .into_iter()
        .map(|r| {
            Map::from_iter([
                ("id".to_string(), Value::from(r.get::<i32, _>(0))),
                (
                    "fg_color".to_string(),
                    Value::from(r.get::<String, _>(1)),
                ),
                (
                    "bg_color".to_string(),
                    Value::from(r.get::<String, _>(2)),
                ),
                (
                    "caption".to_string(),
                    Value::from(r.get::<String, _>(3)),
                ),
            ])
        })
        .collect()
}

/// `Pref_Labels::getlabeltree` — a single `root` node ("Labels") whose children
/// are the owner's labels, each wrapped as `{type, id:"LABEL:n", bare_id, name,
/// fg_color, bg_color, checkbox:false}`, ordered by caption.
pub async fn get_label_tree(pool: &sqlx::PgPool, owner_uid: i64) -> Value {
    let labels = get_all(pool, owner_uid).await;
    let items: Vec<Value> = labels
        .iter()
        .map(|l| {
            let id = l.get("id").and_then(Value::as_i64).unwrap_or(0);
            json!({
                "type": "label",
                "id": format!("LABEL:{}", id),
                "bare_id": id,
                "name": l.get("caption").cloned().unwrap_or_default(),
                "fg_color": l.get("fg_color").cloned().unwrap_or_default(),
                "bg_color": l.get("bg_color").cloned().unwrap_or_default(),
                "checkbox": false,
            })
        })
        .collect();
    json!({
        "identifier": "id",
        "label": "name",
        "items": [{
            "id": "root",
            "name": "Labels",
            "items": items,
        }],
    })
}

/// `Labels::get_as_hash` — `get_all` keyed by label id.
pub async fn get_as_hash(pool: &sqlx::PgPool, owner_uid: i64) -> HashMap<i32, Map<String, Value>> {
    get_all(pool, owner_uid)
        .await
        .into_iter()
        .map(|m| {
            let id = m.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
            (id, m)
        })
        .collect()
}

/// `Labels::update_cache`: (re)write the label cache of article `id`.
/// `labels` is `Article::_get_labels`' row format (`[feed_id, caption,
/// fg_color, bg_color]`); an empty slice re-fetches via
/// `Article::_get_labels` (which uses the session uid). `force` clears the
/// cache first.
pub async fn update_cache(
    db: &Db,
    session_uid: i64,
    owner_uid: i64,
    id: i32,
    labels: &[Value],
    force: bool,
) {
    let pool = db.pool();
    if force {
        clear_cache(pool, id).await;
    }
    let cached = if labels.is_empty() {
        // PHP: `if (!$labels) $labels = Article::_get_labels($id);` — the
        // refetch uses the session uid.
        get_labels_json(pool, session_uid, id).await
    } else {
        Value::Array(labels.to_vec()).to_string()
    };
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = $1
         WHERE ref_id = $2 AND owner_uid = $3",
    )
    .bind(&cached)
    .bind(id)
    .bind(owner_uid)
    .execute(pool)
    .await;
}

/// PHP `empty()` on a decoded JSON value (the `empty($tmp)` in
/// `Article::_get_labels`' cache check).
pub(crate) fn php_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_i64().is_some_and(|i| i == 0),
        Value::String(s) => s.is_empty() || s == "0",
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// `Article::_get_labels` as its `json_encode` result: the JSON of the
/// label rows (or `[]`), with the same cache read/write-back semantics —
/// a usable cache value is returned re-encoded (no write-back), otherwise
/// the hard path runs and the rows or the `{"no-labels": 1}` marker is
/// written back. (This is the string-level form; `article::get_labels` is
/// the typed `Vec<Value>` form on top of it.)
pub async fn get_labels_json(pool: &sqlx::PgPool, owner_uid: i64, id: i32) -> String {
    let cache: Option<String> = sqlx::query_scalar(
        "SELECT label_cache FROM ttrss_user_entries WHERE ref_id = $1 AND owner_uid = $2",
    )
    .bind(id)
    .bind(owner_uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if let Some(cache) = cache.as_deref().filter(|c| !c.is_empty()) {
        // Non-empty cache: PHP `Article::_get_labels` returns immediately —
        // the decoded value if usable, else `[]` (invalid/empty/no-labels).
        // Only an empty/NULL cache falls through to the hard way below.
        match serde_json::from_str::<Value>(cache) {
            Ok(v) if !php_empty_value(&v) && v.get("no-labels").and_then(Value::as_i64) != Some(1) => {
                return v.to_string();
            }
            _ => return Value::Array(Vec::new()).to_string(),
        }
    }

    // The hard way: the user's labels assigned to this article.
    let rows = sqlx::query(
        "SELECT DISTINCT label_id, caption, fg_color, bg_color
         FROM ttrss_labels2, ttrss_user_labels2
         WHERE id = label_id AND article_id = $1 AND owner_uid = $2
         ORDER BY caption",
    )
    .bind(id)
    .bind(owner_uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut rv: Vec<Value> = Vec::new();
    for row in &rows {
        let label_id: i32 = row.get(0);
        let caption: String = row.get(1);
        let fg_color: String = row.get(2);
        let bg_color: String = row.get(3);
        rv.push(json!([label_to_feed_id(label_id as i64), caption, fg_color, bg_color]));
    }

    // PHP `Article::_get_labels` writes back the rows or the marker.
    let write: Value = if rv.is_empty() {
        json!({ "no-labels": 1 })
    } else {
        Value::Array(rv.clone())
    };
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET label_cache = $1 WHERE ref_id = $2 AND owner_uid = $3",
    )
    .bind(write.to_string())
    .bind(id)
    .bind(owner_uid)
    .execute(pool)
    .await;

    // The value `Article::_get_labels` returns (its `json_encode` form).
    Value::Array(rv).to_string()
}

/// `Labels::clear_cache`: blank the label cache of article `id`.
pub async fn clear_cache(pool: &sqlx::PgPool, id: i32) {
    let _ = sqlx::query("UPDATE ttrss_user_entries SET label_cache = '' WHERE ref_id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

/// `Labels::remove_article`: detach article `id` from the label `caption`
/// (no-op when the label does not exist), then clear the article's cache.
pub async fn remove_article(pool: &sqlx::PgPool, id: i32, label: &str, owner_uid: i64) {
    let label_id = find_id(pool, label, owner_uid).await;
    if label_id == 0 {
        return;
    }
    let _ = sqlx::query(
        "DELETE FROM ttrss_user_labels2
         WHERE label_id = $1 AND article_id = $2",
    )
    .bind(label_id as i64)
    .bind(id)
    .execute(pool)
    .await;
    clear_cache(pool, id).await;
}

/// `Labels::add_article`: attach article `id` to the label `caption`
/// (idempotent; no-op when the label does not exist), then clear the
/// article's cache.
pub async fn add_article(pool: &sqlx::PgPool, id: i32, label: &str, owner_uid: i64) {
    let label_id = find_id(pool, label, owner_uid).await;
    if label_id == 0 {
        return;
    }
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_labels2, ttrss_user_labels2
         WHERE label_id = id AND label_id = $1 AND article_id = $2
         AND owner_uid = $3",
    )
    .bind(label_id as i64)
    .bind(id)
    .bind(owner_uid)
    .fetch_one(pool)
    .await
    .ok()
    .flatten();
    if exists.unwrap_or(0) == 0 {
        let _ = sqlx::query(
            "INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES ($1, $2)",
        )
        .bind(label_id as i64)
        .bind(id)
        .execute(pool)
        .await;
    }
    clear_cache(pool, id).await;
}

/// `Labels::remove`: delete label `id` for the owner; when a row was removed
/// also drop the label's access key and clear the owner's label caches.
/// `owner_uid` is the session uid when `None`/0.
pub async fn remove(db: &Db, id: i32, session_uid: i64, owner_uid: Option<i64>) -> Result<(), sqlx::Error> {
    let owner = match owner_uid {
        Some(u) if u != 0 => u,
        _ => session_uid,
    };
    let pool = db.pool();
    let mut tx = pool.begin().await?;

    // PHP reads the caption unconditionally (a NULL row just yields no
    // cached-data cleanup).
    let caption: Option<String> = sqlx::query_scalar(
        "SELECT caption FROM ttrss_labels2 WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();

    let result = sqlx::query("DELETE FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2")
        .bind(id)
        .bind(owner)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() != 0 && caption.is_some() {
        // Remove the label's API access key + cached data. (`feed_id` is a
        // varchar column — PHP's PDO binds the int as a string.)
        let ext_id = label_to_feed_id(id as i64);
        sqlx::query("DELETE FROM ttrss_access_keys WHERE feed_id = $1 AND owner_uid = $2")
            .bind(ext_id.to_string())
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE ttrss_user_entries SET label_cache = '' WHERE owner_uid = $1")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// `Labels::create`: insert a new label, or `None` when an equal caption
/// (case-insensitive) already exists for the owner (PHP `false`). `fg_color`
/// / `bg_color` default to `""` (PHP's `''` default), and `owner_uid` to the
/// session uid.
pub async fn create(
    db: &Db,
    caption: &str,
    fg_color: Option<&str>,
    bg_color: Option<&str>,
    session_uid: i64,
    owner_uid: Option<i64>,
) -> Result<Option<i64>, sqlx::Error> {
    let owner = match owner_uid {
        Some(u) if u != 0 => u,
        _ => session_uid,
    };
    let pool = db.pool();
    let mut tx = pool.begin().await?;

    let existing: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_labels2
         WHERE LOWER(caption) = LOWER($1) AND owner_uid = $2",
    )
    .bind(caption)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?;

    let result = if existing.is_none() {
        let rows = sqlx::query(
            "INSERT INTO ttrss_labels2 (caption, owner_uid, fg_color, bg_color)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(caption)
        .bind(owner)
        .bind(fg_color.unwrap_or(""))
        .bind(bg_color.unwrap_or(""))
        .execute(&mut *tx)
        .await?;
        Some(rows.rows_affected() as i64)
    } else {
        None // PHP `false`
    };

    tx.commit().await?;

    Ok(result)
}
