//! Port of `classes/Counters.php` + `RPC::getAllCounters` — the global,
//! virtual-feed, label, feed, and category counter list the SPA polls.
//!
//! Row *order* matters: the PHP emits no `ORDER BY` on the aggregate queries
//! and the SPA (and the harness) see the natural PG order. The session setup
//! in `ttrss_core::db` reproduces `Db::pdo_connect()`'s
//! `set cpu_tuple_cost = 0.5`, so the same query text yields the same plan
//! and the same row order as the PHP oracle.

use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::db::Db;

use crate::prefs;
use crate::headlines::PLUGIN_FEED_BASE_INDEX;
use crate::labels::{feed_to_label_id, label_to_feed_id, LABEL_BASE_INDEX};
use crate::session::Sess;
use crate::time::TzCtx;
use crate::AppState;
use crate::util::php_int_cast;

// `Feeds::*` (functions.php).

const FEED_ARCHIVED: i64 = 0;
const FEED_STARRED: i64 = -1;
const FEED_PUBLISHED: i64 = -2;
const FEED_FRESH: i64 = -3;
const FEED_ALL: i64 = -4;
const FEED_RECENTLY_READ: i64 = -6;
const CAT_LABELS: i64 = -2;

/// `RPC::getAllCounters`.
pub async fn get_all_counters(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;

    // `@$seq = (int) $_REQUEST['seq']`
    let seq = extra
        .get("seq")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);

    // `feed_id_count == -1 ? null : _param_to_int_array(...)` — "return all
    // counters" when absent.
    let feed_ids = parse_id_list(extra.get("feed_id_count"), extra.get("feed_ids"));
    let label_ids = parse_id_list(extra.get("label_id_count"), extra.get("label_ids"));

    let disable_conditional =
        prefs::resolve("DISABLE_CONDITIONAL_COUNTERS", &rows, profile).as_bool();

    let client_tz_offset = data.get_int("clientTzOffset").unwrap_or(0);

    // `get_conditional` passes `Feeds::_cats_of($feed_ids, uid, true)` as the
    // category list when feeds were given.
    let cat_ids = match feed_ids.as_ref() {
        Some(ids) if !disable_conditional && !ids.is_empty() => {
            cats_of(pool, uid, ids, true).await
        }
        _ => Vec::new(),
    };

    let counters: Vec<Value> = match &feed_ids {
        Some(ids) if !disable_conditional => all_counters(
            pool,
            uid,
            profile,
            &rows,
            &state.config.feed_icon_dir,
            client_tz_offset,
            Some(ids),
            label_ids.as_deref(),
            Some(&cat_ids),
        )
        .await,
        _ => all_counters(
            pool,
            uid,
            profile,
            &rows,
            &state.config.feed_icon_dir,
            client_tz_offset,
            None,
            None,
            None,
        )
        .await,
    };

    json!({ "counters": counters, "seq": seq })
}

/// `Handler::_param_to_int_array`: a CSV string of ints (or null). A
/// non-integer item rejects the whole input (null). `*_count == -1` (absent)
/// → null (no list); `*_count >= 0` → the list (possibly empty).
fn parse_id_list(count: Option<&Value>, ids: Option<&Value>) -> Option<Vec<i64>> {
    let count = count
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(-1);
    if count == -1 {
        return None;
    }
    let s = ids.and_then(|v| v.as_str()).unwrap_or("");
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let items: Vec<&str> = trimmed
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if items.is_empty() {
        return Some(Vec::new());
    }
    let parsed: Vec<i64> = items
        .iter()
        .map(|t| t.parse::<i64>())
        .collect::<Result<_, _>>()
        .ok()?;
    Some(parsed)
}

/// `Counters::get_all` / `get_conditional` body (faithful port — the PHP
/// method takes this many parameters).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn all_counters(
    pool: &sqlx::PgPool,
    uid: i64,
    profile: Option<i32>,
    rows: &[(String, Option<i32>, String)],
    icon_dir: &std::path::Path,
    client_tz_offset: i64,
    feed_ids: Option<&[i64]>,
    label_ids: Option<&[i64]>,
    cat_ids: Option<&[i64]>,
) -> Vec<Value> {
    let fresh_max_age = prefs::resolve("FRESH_ARTICLE_MAX_AGE", rows, profile).as_int();

    let mut out: Vec<Value> = Vec::new();
    out.extend(get_global(pool, uid).await);
    out.extend(get_virt(pool, uid, fresh_max_age).await);
    out.extend(labels_counters(pool, uid, label_ids).await);
    out.extend(get_feeds(pool, uid, feed_ids, icon_dir, rows, profile, client_tz_offset).await);
    out.extend(get_cats(pool, uid, cat_ids, fresh_max_age).await);
    out
}

/// `Counters::get_global`.
async fn get_global(pool: &sqlx::PgPool, uid: i64) -> Vec<Value> {
    let global_unread = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(CASE WHEN unread THEN 1 ELSE 0 END), 0)
         FROM ttrss_user_entries ue WHERE ue.owner_uid = $1",
    )
    .bind(uid)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let subscribed = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = $1")
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    vec![
        json!({ "id": "global-unread", "counter": global_unread }),
        json!({ "id": "subscribed-feeds", "counter": subscribed }),
    ]
}

/// `Counters::get_virt` (the five built-in virtual feeds; no plugin-provided
/// virtual feeds in this deployment).
async fn get_virt(pool: &sqlx::PgPool, uid: i64, fresh_max_age: i64) -> Vec<Value> {
    let mut out = Vec::new();
    for feed_id in [FEED_ARCHIVED, FEED_STARRED, FEED_PUBLISHED, FEED_FRESH, FEED_ALL] {
        let count = feed_counters(pool, uid, feed_id, false, true, true, fresh_max_age).await;
        let auxctr = if [FEED_ARCHIVED, FEED_STARRED, FEED_PUBLISHED].contains(&feed_id) {
            feed_counters(pool, uid, feed_id, false, false, true, fresh_max_age).await
        } else {
            0
        };
        let mut cv = Map::new();
        cv.insert("id".into(), json!(feed_id));
        cv.insert("counter".into(), json!(count));
        cv.insert("auxcounter".into(), json!(auxctr));
        if feed_id == FEED_STARRED {
            cv.insert("markedcounter".into(), json!(auxctr));
        } else if feed_id == FEED_PUBLISHED {
            cv.insert("publishedcounter".into(), json!(auxctr));
        }
        out.push(Value::Object(cv));
    }
    out
}

/// `Counters::get_labels` (PHP body).
async fn labels_counters(
    pool: &sqlx::PgPool,
    uid: i64,
    label_ids: Option<&[i64]>,
) -> Vec<Value> {
    let rows = match label_ids {
        Some([]) => return Vec::new(),
        Some(ids) => {
            let qmarks: Vec<String> = (3..=2 + ids.len())
                .map(|i| format!("${i}"))
                .collect();
            let q = format!(
                "SELECT id, caption,
                    SUM(CASE WHEN u1.unread = true THEN 1 ELSE 0 END) AS count_unread,
                    SUM(CASE WHEN u1.marked = true THEN 1 ELSE 0 END) AS count_marked,
                    SUM(CASE WHEN u1.published = true THEN 1 ELSE 0 END) AS count_published,
                    COUNT(u1.unread) AS total
                FROM ttrss_labels2 LEFT JOIN ttrss_user_labels2 ON
                    (ttrss_labels2.id = label_id)
                        LEFT JOIN ttrss_user_entries AS u1 ON u1.ref_id = article_id AND u1.owner_uid = $1
                            WHERE ttrss_labels2.owner_uid = $2 AND ttrss_labels2.id IN ({})
                                GROUP BY ttrss_labels2.id, ttrss_labels2.caption",
                qmarks.join(",")
            );
            let mut query = sqlx::query(&q);
            query = query.bind(uid); // u1.owner_uid (ON clause)
            query = query.bind(uid); // ttrss_labels2.owner_uid (WHERE clause)
            for id in ids {
                query = query.bind(*id);
            }
            query.fetch_all(pool).await.unwrap_or_default()
        }
        _ => sqlx::query(
            "SELECT id, caption,
                SUM(CASE WHEN u1.unread = true THEN 1 ELSE 0 END) AS count_unread,
                SUM(CASE WHEN u1.marked = true THEN 1 ELSE 0 END) AS count_marked,
                SUM(CASE WHEN u1.published = true THEN 1 ELSE 0 END) AS count_published,
                COUNT(u1.unread) AS total
            FROM ttrss_labels2 LEFT JOIN ttrss_user_labels2 ON
                (ttrss_labels2.id = label_id)
                    LEFT JOIN ttrss_user_entries AS u1 ON u1.ref_id = article_id AND u1.owner_uid = $1
                        WHERE ttrss_labels2.owner_uid = $1
                            GROUP BY ttrss_labels2.id, ttrss_labels2.caption",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default(),
    };

    rows.iter()
        .map(|r| {
            let label_id = r.get::<i32, _>(0) as i64;
            // SUM columns are NULL for labels without articles; PHP casts to 0.
            let mut cv = Map::new();
            cv.insert("id".into(), json!(label_to_feed_id(label_id)));
            cv.insert("counter".into(), json!(r.get::<Option<i64>, _>(2).unwrap_or(0)));
            cv.insert("auxcounter".into(), json!(r.get::<i64, _>(5)));
            cv.insert("markedcounter".into(), json!(r.get::<Option<i64>, _>(3).unwrap_or(0)));
            cv.insert("publishedcounter".into(), json!(r.get::<Option<i64>, _>(4).unwrap_or(0)));
            cv.insert("description".into(), json!(r.get::<String, _>(1)));
            Value::Object(cv)
        })
        .collect()
}

/// `Counters::get_feeds` (the idiorm query, verbatim).
async fn get_feeds(
    pool: &sqlx::PgPool,
    uid: i64,
    feed_ids: Option<&[i64]>,
    icon_dir: &std::path::Path,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
    client_tz_offset: i64,
) -> Vec<Value> {
    if matches!(feed_ids, Some(ids) if ids.is_empty()) {
        return Vec::new();
    }

    let tz = TzCtx {
        user_tz: prefs::resolve("USER_TIMEZONE", rows, profile).as_str().to_string(),
        client_tz_offset,
        short_date_format: prefs::resolve("SHORT_DATE_FORMAT", rows, profile).as_str().to_string(),
        long_date_format: prefs::resolve("LONG_DATE_FORMAT", rows, profile).as_str().to_string(),
    };

    let q: String = match feed_ids {
        Some(ids) => {
            let qmarks: Vec<String> = (2..=1 + ids.len()).map(|i| format!("${i}")).collect();
            format!(
                "SELECT f.id, f.title, f.last_error,
                    SUM(CASE WHEN ue.unread THEN 1 ELSE 0 END) AS count,
                    SUM(CASE WHEN ue.marked THEN 1 ELSE 0 END) AS count_marked,
                    SUM(CASE WHEN ue.published THEN 1 ELSE 0 END) AS count_published,
                    SUBSTRING_FOR_DATE(f.last_updated,1,19) AS last_updated
                FROM ttrss_feeds AS f
                INNER JOIN ttrss_user_entries AS ue ON (ue.feed_id = f.id)
                WHERE (ue.owner_uid = $1) AND f.id IN ({})
                GROUP BY f.id",
                qmarks.join(",")
            )
        }
        _ => "SELECT f.id, f.title, f.last_error,
                    SUM(CASE WHEN ue.unread THEN 1 ELSE 0 END) AS count,
                    SUM(CASE WHEN ue.marked THEN 1 ELSE 0 END) AS count_marked,
                    SUM(CASE WHEN ue.published THEN 1 ELSE 0 END) AS count_published,
                    SUBSTRING_FOR_DATE(f.last_updated,1,19) AS last_updated
                FROM ttrss_feeds AS f
                INNER JOIN ttrss_user_entries AS ue ON (ue.feed_id = f.id)
                WHERE (ue.owner_uid = $1)
                GROUP BY f.id".into(),
    };
    let mut query = sqlx::query(&q);
    query = query.bind(uid);
    if let Some(ids) = feed_ids {
        for id in ids {
            query = query.bind(*id);
        }
    }
    let rows = query.fetch_all(pool).await.unwrap_or_default();

    rows.iter()
        .map(|r| {
            let feed_id = r.get::<i32, _>(0) as i64;
            let title: String = r.get(1);
            let last_error: String = r.get(2);
            let count: i64 = r.get(3);
            let count_marked: i64 = r.get(4);
            let count_published: i64 = r.get(5);
            let last_updated: Option<String> = r.get(6);
            let updated = crate::time::make_local_datetime(
                last_updated.as_deref(),
                false,
                &tz,
            );
            let has_icon = icon_dir.join(feed_id.to_string()).is_file();
            let ts = if has_icon {
                icon_dir
                    .join(feed_id.to_string())
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            } else {
                0
            };
            let mut out = Map::new();
            out.insert("id".into(), json!(feed_id));
            out.insert("title".into(), json!(truncate_string(&title, 30)));
            out.insert("error".into(), json!(last_error));
            out.insert("updated".into(), json!(updated));
            out.insert("counter".into(), json!(count));
            out.insert("markedcounter".into(), json!(count_marked));
            out.insert("publishedcounter".into(), json!(count_published));
            out.insert("ts".into(), json!(ts));
            Value::Object(out)
        })
        .collect()
}

/// `Counters::get_cats` — the Labels category entry, then the UNION of real
/// categories + uncategorized (natural PG order), each with its subtree
/// counters added.
async fn get_cats(
    pool: &sqlx::PgPool,
    uid: i64,
    cat_ids: Option<&[i64]>,
    fresh_max_age: i64,
) -> Vec<Value> {
    let mut out: Vec<Value> = vec![json!({
        "id": CAT_LABELS,
        "kind": "cat",
        "counter": cat_unread(pool, uid, CAT_LABELS, fresh_max_age).await,
    })];

    let rows = match cat_ids {
        Some([]) => return Vec::new(),
        Some(ids) => {
            let qmarks: Vec<String> = (2..=1 + ids.len()).map(|i| format!("${i}")).collect();
            let q = format!(
                "SELECT fc.id,
                        SUM(CASE WHEN unread THEN 1 ELSE 0 END) AS count,
                        SUM(CASE WHEN marked THEN 1 ELSE 0 END) AS count_marked,
                        SUM(CASE WHEN published THEN 1 ELSE 0 END) AS count_published,
                        (SELECT COUNT(id) FROM ttrss_feed_categories fcc
                            WHERE fcc.parent_cat = fc.id) AS num_children
                    FROM ttrss_feed_categories fc
                        LEFT JOIN ttrss_feeds f ON (f.cat_id = fc.id)
                        LEFT JOIN ttrss_user_entries ue ON (ue.feed_id = f.id)
                    WHERE fc.owner_uid = $1 AND fc.id IN ({})
                    GROUP BY fc.id
                UNION
                    SELECT 0,
                        SUM(CASE WHEN unread THEN 1 ELSE 0 END) AS count,
                        SUM(CASE WHEN marked THEN 1 ELSE 0 END) AS count_marked,
                        SUM(CASE WHEN published THEN 1 ELSE 0 END) AS count_published,
                        0
                    FROM ttrss_feeds f, ttrss_user_entries ue
                    WHERE f.cat_id IS NULL AND
                        ue.feed_id = f.id AND
                        ue.owner_uid = ${}$",
                qmarks.join(","),
                2 + ids.len()
            );
            let mut query = sqlx::query(&q);
            query = query.bind(uid);
            for id in ids {
                query = query.bind(*id);
            }
            query = query.bind(uid);
            query.fetch_all(pool).await.unwrap_or_default()
        }
        _ => sqlx::query(
            "SELECT fc.id,
                    SUM(CASE WHEN unread THEN 1 ELSE 0 END) AS count,
                    SUM(CASE WHEN marked THEN 1 ELSE 0 END) AS count_marked,
                    SUM(CASE WHEN published THEN 1 ELSE 0 END) AS count_published,
                    (SELECT COUNT(id) FROM ttrss_feed_categories fcc
                        WHERE fcc.parent_cat = fc.id) AS num_children
                FROM ttrss_feed_categories fc
                    LEFT JOIN ttrss_feeds f ON (f.cat_id = fc.id)
                    LEFT JOIN ttrss_user_entries ue ON (ue.feed_id = f.id)
                WHERE fc.owner_uid = $1
                GROUP BY fc.id
            UNION
                SELECT 0,
                    SUM(CASE WHEN unread THEN 1 ELSE 0 END) AS count,
                    SUM(CASE WHEN marked THEN 1 ELSE 0 END) AS count_marked,
                    SUM(CASE WHEN published THEN 1 ELSE 0 END) AS count_published,
                    0
                FROM ttrss_feeds f, ttrss_user_entries ue
                WHERE f.cat_id IS NULL AND
                    ue.feed_id = f.id AND
                    ue.owner_uid = $1",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default(),
    };

    for r in &rows {
        let cat_id = r.get::<i32, _>(0) as i64;
        // SUM columns are NULL for cats without feeds; PHP casts to 0.
        let count = r.get::<Option<i64>, _>(1).unwrap_or(0);
        let count_marked = r.get::<Option<i64>, _>(2).unwrap_or(0);
        let count_published = r.get::<Option<i64>, _>(3).unwrap_or(0);
        let num_children = r.get::<i64, _>(4);

        let (child_counter, child_marked, child_published) =
            if num_children > 0 {
                cat_children(pool, uid, cat_id, fresh_max_age).await
            } else {
                (0, 0, 0)
            };

        let mut out_r = Map::new();
        out_r.insert("id".into(), json!(cat_id));
        out_r.insert("kind".into(), json!("cat"));
        out_r.insert("markedcounter".into(), json!(count_marked + child_marked));
        out_r.insert("publishedcounter".into(), json!(count_published + child_published));
        out_r.insert("counter".into(), json!(count + child_counter));
        out.push(Value::Object(out_r));
    }
    out
}

/// `Counters::get_cat_children` — recursive (subtree) counters.
async fn cat_children(
    pool: &sqlx::PgPool,
    uid: i64,
    cat_id: i64,
    fresh_max_age: i64,
) -> (i64, i64, i64) {
    let children = sqlx::query("SELECT id FROM ttrss_feed_categories WHERE parent_cat = $1 AND owner_uid = $2")
        .bind(cat_id)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut unread = 0i64;
    let mut marked = 0i64;
    let mut published = 0i64;
    for c in children {
        let cid = c.get::<i32, _>(0) as i64;
        let (tmp_u, tmp_m, tmp_p) =
            Box::pin(cat_children(pool, uid, cid, fresh_max_age)).await;
        unread += tmp_u + cat_unread(pool, uid, cid, fresh_max_age).await;
        marked += tmp_m + cat_marked(pool, uid, cid, fresh_max_age).await;
        published += tmp_p + cat_published(pool, uid, cid, fresh_max_age).await;
    }
    (unread, marked, published)
}

/// `Feeds::_get_counters` (the general form; `is_cat` routes to the cat
/// unread counters). `feed_is_int` is PHP's `$n_feed === Feeds::FEED_ARCHIVED`
/// strictness: string feeds (e.g. `"0"`) take `feed_id = 0`, not
/// `feed_id IS NULL`.
async fn feed_counters(
    pool: &sqlx::PgPool,
    uid: i64,
    feed: i64,
    is_cat: bool,
    unread_only: bool,
    feed_is_int: bool,
    fresh_max_age: i64,
) -> i64 {
    if is_cat {
        return cat_unread(pool, uid, feed, fresh_max_age).await;
    }
    if feed < PLUGIN_FEED_BASE_INDEX && feed > LABEL_BASE_INDEX {
        // plugin virtual feeds — none in this deployment
        return 0;
    }
    if feed == FEED_RECENTLY_READ {
        return 0;
    }

    let unread_qpart = if unread_only { "unread = true" } else { "true" };

    let (match_part, need_entries) = match feed {
        FEED_STARRED => ("marked = true".to_string(), false),
        FEED_PUBLISHED => ("published = true".to_string(), false),
        FEED_FRESH => (
            format!(
                "unread = true AND score >= 0 AND date_entered > NOW() - INTERVAL '{fresh_max_age} hour'"
            ),
            true,
        ),
        FEED_ALL => ("true".to_string(), false),
        n if n >= 0 => {
            if n == FEED_ARCHIVED && feed_is_int {
                ("feed_id IS NULL".to_string(), false)
            } else {
                (format!("feed_id = {n}"), false)
            }
        }
        n if n < LABEL_BASE_INDEX => {
            // labels — handled by the dedicated counter below
            return label_unread(pool, uid, feed_to_label_id(n)).await;
        }
        _ => return 0,
    };

    let sql = if need_entries {
        format!(
            "SELECT count(int_id) AS unread FROM ttrss_user_entries,ttrss_entries
             WHERE ttrss_entries.id = ttrss_user_entries.ref_id
               AND {unread_qpart} AND ({match_part}) AND ttrss_user_entries.owner_uid = $1"
        )
    } else {
        format!(
            "SELECT count(int_id) AS unread FROM ttrss_user_entries
             WHERE {unread_qpart} AND ({match_part}) AND ttrss_user_entries.owner_uid = $1"
        )
    };
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

/// `Feeds::_get_counters(int|string $feed, …)` — a feed id **or** a tag name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeedOrTag<'a> {
    /// A numeric feed id (real, virtual, or label reference).
    Feed(i64),
    /// A tag name (`ttrss_tags.tag_name`).
    Tag(&'a str),
}

impl FeedOrTag<'_> {
    /// PHP `(int) $feed`.
    pub fn as_int(self) -> i64 {
        match self {
            FeedOrTag::Feed(n) => n,
            FeedOrTag::Tag(s) => php_int_cast(s),
        }
    }
}

/// PHP `is_numeric()` (the form relevant here: optional sign, digits and a
/// single dot; no exponents needed by the callers).
fn php_is_numeric(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    !t.is_empty()
        && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && t.bytes().any(|b| b.is_ascii_digit())
}

/// `Feeds::_get_counters` (the tag branch of the `$feed != "0" &&
/// $n_feed == 0` case).
async fn tag_counters(pool: &sqlx::PgPool, uid: i64, unread_only: bool, tag: &str) -> i64 {
    let unread_qpart = if unread_only { "unread = true" } else { "true" };
    let n: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT SUM((SELECT COUNT(int_id)
            FROM ttrss_user_entries,ttrss_entries WHERE int_id = post_int_id
                AND ref_id = id AND {unread_qpart})) AS count FROM ttrss_tags
            WHERE owner_uid = $1 AND tag_name = $2"
    ))
    .bind(uid)
    .bind(tag)
    .fetch_one(pool)
    .await
    .unwrap_or(None);
    // `SUM()` is NULL when the tag has no rows.
    n.unwrap_or(0)
}

/// `Feeds::_get_counters` (public form of the PHP static; the session uid is
/// explicit, the session profile is unavailable in this context so the
/// `FRESH_ARTICLE_MAX_AGE` pref resolves against the default profile —
/// matching the integration-test session, which carries no profile).
pub async fn get_counters(
    db: &Db,
    uid: i64,
    feed: FeedOrTag<'_>,
    is_cat: bool,
    unread_only: bool,
    owner_uid: Option<i64>,
) -> i64 {
    let pool = db.pool();
    let n_feed = feed.as_int();
    // `if (!$owner_uid) $owner_uid = $_SESSION["uid"]`
    let owner = match owner_uid {
        Some(u) if u != 0 => u,
        _ => uid,
    };
    if is_cat {
        return cat_unread(pool, owner, n_feed, 0).await;
    }
    // `is_numeric($feed) && $feed < PLUGIN_FEED_BASE_INDEX && $feed >
    // LABEL_BASE_INDEX` — plugin virtual feeds (none in this deployment).
    let numeric = matches!(feed, FeedOrTag::Feed(_))
        || matches!(feed, FeedOrTag::Tag(s) if php_is_numeric(s));
    if numeric && n_feed < PLUGIN_FEED_BASE_INDEX && n_feed > LABEL_BASE_INDEX {
        return 0;
    }
    if n_feed == FEED_RECENTLY_READ {
        return 0;
    }
    // tags: `$feed != "0" && $n_feed == 0`
    if let FeedOrTag::Tag(s) = feed {
        if s != "0" && n_feed == 0 {
            return tag_counters(pool, owner, unread_only, s).await;
        }
    }
    let fresh_max_age = if n_feed == FEED_FRESH {
        let rows = prefs::load_rows(db, owner, None).await;
        prefs::resolve("FRESH_ARTICLE_MAX_AGE", &rows, None).as_int()
    } else {
        0
    };
    feed_counters(pool, owner, n_feed, false, unread_only, matches!(feed, FeedOrTag::Feed(_)), fresh_max_age).await
}

/// `Counters::get_all` (PHP) — the full counter list for the session user.
pub async fn get_all(db: &Db, uid: i64, icon_dir: &std::path::Path) -> Vec<Value> {
    let rows = prefs::load_rows(db, uid, None).await;
    all_counters(db.pool(), uid, None, &rows, icon_dir, 0, None, None, None).await
}

/// `Counters::get_conditional` (PHP) — global + virtual + the given labels
/// and feeds + the categories of the given feeds.
pub async fn get_conditional(
    db: &Db,
    uid: i64,
    feed_ids: Option<&[i64]>,
    label_ids: Option<&[i64]>,
    icon_dir: &std::path::Path,
) -> Vec<Value> {
    let rows = prefs::load_rows(db, uid, None).await;
    // `is_array($feed_ids) ? Feeds::_cats_of($feed_ids, uid, true) : null`
    // (an empty list yields an empty category list, not the full one).
    let cat_ids: Option<Vec<i64>> = match feed_ids {
        Some(ids) => {
            Some(if ids.is_empty() {
                Vec::new()
            } else {
                cats_of(db.pool(), uid, ids, true).await
            })
        }
        None => None,
    };
    all_counters(
        db.pool(),
        uid,
        None,
        &rows,
        icon_dir,
        0,
        feed_ids,
        label_ids,
        cat_ids.as_deref(),
    )
    .await
}

/// `Counters::get_labels` (PHP) — the label counter entries (all labels when
/// `label_ids` is `None`, the given subset otherwise).
pub async fn get_labels(db: &Db, uid: i64, label_ids: Option<&[i64]>) -> Vec<Value> {
    labels_counters(db.pool(), uid, label_ids).await
}

/// `Feeds::_get_label_unread`.
async fn label_unread(pool: &sqlx::PgPool, uid: i64, label_id: i64) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT article_id) FROM ttrss_user_entries ue, ttrss_user_labels2 l
         WHERE article_id = ref_id AND unread IS true AND ue.owner_uid = $1 AND l.label_id = $2",
    )
    .bind(uid)
    .bind(label_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

/// `Feeds::_get_cat_unread` (real cats + the special LABELS/UNCATEGORIZED ids).
async fn cat_unread(
    pool: &sqlx::PgPool,
    uid: i64,
    cat: i64,
    _fresh_max_age: i64,
) -> i64 {
    if cat >= 0 {
        let n = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT SUM(CASE WHEN unread THEN 1 ELSE 0 END)
             FROM ttrss_user_entries
             WHERE feed_id IN (SELECT id FROM ttrss_feeds
                WHERE (cat_id = $2 OR ($2 IS NULL AND cat_id IS NULL))
                AND owner_uid = $1)
             AND owner_uid = $1",
        )
        .bind(uid)
        .bind(if cat == 0 { None } else { Some(cat as i32 )})
        .fetch_one(pool)
        .await
        .unwrap_or(None);
        return n.unwrap_or(0);
    } else if cat == CAT_LABELS {
        return sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT article_id)
             FROM ttrss_user_entries ue, ttrss_user_labels2 l
             WHERE article_id = ref_id AND unread IS true AND ue.owner_uid = $1",
        )
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    }
    0
}

/// `Feeds::_get_cat_marked`.
async fn cat_marked(pool: &sqlx::PgPool, uid: i64, cat: i64, _fresh_max_age: i64) -> i64 {
    if cat >= 0 {
        let n = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT SUM(CASE WHEN marked THEN 1 ELSE 0 END)
             FROM ttrss_user_entries
             WHERE feed_id IN (SELECT id FROM ttrss_feeds
                WHERE (cat_id = $2 OR ($2 IS NULL AND cat_id IS NULL))
                AND owner_uid = $1)
             AND owner_uid = $1",
        )
        .bind(uid)
        .bind(if cat == 0 { None } else { Some(cat as i32) })
        .fetch_one(pool)
        .await
        .unwrap_or(None);
        return n.unwrap_or(0);
    }
    0
}

/// `Feeds::_get_cat_published`.
async fn cat_published(pool: &sqlx::PgPool, uid: i64, cat: i64, _fresh_max_age: i64) -> i64 {
    if cat >= 0 {
        let n = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT SUM(CASE WHEN published THEN 1 ELSE 0 END)
             FROM ttrss_user_entries
             WHERE feed_id IN (SELECT id FROM ttrss_feeds
                WHERE (cat_id = $2 OR ($2 IS NULL AND cat_id IS NULL))
                AND owner_uid = $1)
             AND owner_uid = $1",
        )
        .bind(uid)
        .bind(if cat == 0 { None } else { Some(cat as i32) })
        .fetch_one(pool)
        .await
        .unwrap_or(None);
        return n.unwrap_or(0);
    }
    0
}

/// `Feeds::_cats_of` — distinct cats of the given feeds (+ parent cats when
/// `with_parents`).
async fn cats_of(pool: &sqlx::PgPool, uid: i64, feeds: &[i64], with_parents: bool) -> Vec<i64> {
    if feeds.is_empty() {
        return Vec::new();
    }
    let qmarks: Vec<String> = (2..=1 + feeds.len()).map(|i| format!("${i}")).collect();
    let q = format!(
        "SELECT DISTINCT cat_id, fc.parent_cat FROM ttrss_feeds f LEFT JOIN ttrss_feed_categories fc
            ON (fc.id = f.cat_id)
            WHERE f.owner_uid = $1 AND f.id IN ({})",
        qmarks.join(",")
    );
    let mut query = sqlx::query(&q);
    query = query.bind(uid);
    for id in feeds {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(pool).await.unwrap_or_default();

    let mut rv: Vec<i64> = Vec::new();
    if let Some(r) = rows.first() {
        let cat_id = r.get::<Option<i32>, _>(0).unwrap_or(0) as i64;
        rv.push(cat_id);
        if with_parents {
            if let Some(parent) = r.get::<Option<i32>, _>(1) {
                rv.extend(parent_cats(pool, uid, parent as i64).await);
            }
        }
    }
    // `array_unique` — dedup preserving first-seen order.
    let mut seen = std::collections::BTreeSet::new();
    rv.retain(|c| seen.insert(*c));
    rv
}

/// `Feeds::_get_parent_cats` (recursive).
async fn parent_cats(pool: &sqlx::PgPool, uid: i64, cat: i64) -> Vec<i64> {
    let mut rv = Vec::new();
    let rows = sqlx::query(
        "SELECT parent_cat FROM ttrss_feed_categories
         WHERE id = $1 AND owner_uid = $2 AND parent_cat IS NOT NULL",
    )
    .bind(cat)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for r in rows {
        let p = r.get::<Option<i32>, _>(0).unwrap_or(0) as i64;
        rv.push(p);
        rv.extend(Box::pin(parent_cats(pool, uid, p)).await);
    }
    rv
}

/// `truncate_string($str, 30)` — `&hellip;` suffix.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let cut: String = s.chars().take(max_len).collect();
        format!("{cut}&hellip;")
    } else {
        s.to_string()
    }
}


