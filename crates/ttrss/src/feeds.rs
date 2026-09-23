//! Port of `Pref_Feeds::getfeedtree` + the `Feeds` counter / icon / title
//! helpers it relies on — the feed list the Dojo SPA renders in the left
//! panel.
//!
//! `backend.php?op=Pref_Feeds&method=getfeedtree&mode=2` (a GET, CSRF-ignored)
//! returns a nested dojo `ItemFileReadStore`: `{identifier, label, items:[…]}`.
//! Mode 2 (the SPA's default) lays out the special + label categories and the
//! real categories as a flat top-level list (no `root` node); mode 0 wraps
//! everything in a single `root` node.

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use base64::Engine as _;
use kuchikiki::traits::TendrilSink;
use kuchikiki::parse_html;
use minijinja::Environment;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::Row;
use sqlx::AssertSqlSafe;

use crate::cats::{CAT_LABELS, CAT_SPECIAL};
use crate::controls;
use crate::util;

use crate::labels::LABEL_BASE_INDEX;
use crate::prefs;
use crate::session::Sess;
use crate::time::TzCtx;
use crate::AppState;
use crate::pluginhost::{HOOK_PREFS_EDIT_FEED, HOOK_PREFS_SAVE_FEED};

// `Feeds::*` virtual-feed pseudo-ids (functions.php).
pub(crate) const FEED_ARCHIVED: i64 = 0;
pub(crate) const FEED_STARRED: i64 = -1;
pub(crate) const FEED_PUBLISHED: i64 = -2;
pub(crate) const FEED_FRESH: i64 = -3;
pub(crate) const FEED_ALL: i64 = -4;
pub(crate) const FEED_RECENTLY_READ: i64 = -6;

/// `clean($_REQUEST['mode'] ?? 0) == 2`.
fn is_mode_2(extra: &Map<String, Value>) -> bool {
    extra
        .get("mode")
        .and_then(|v| v.as_str().unwrap_or("").trim().parse::<i64>().ok())
        == Some(2)
}

/// `Pref_Feeds::feedsWithErrors` — the feeds with a non-empty `last_error`.
/// No `ORDER BY`: the natural plan order is what the oracle emits.
pub async fn feeds_with_errors(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let rows = sqlx::query(
        "SELECT id, title, feed_url, last_error, site_url FROM ttrss_feeds \n\n         WHERE last_error != '' AND owner_uid = $1 AND update_interval >= 0",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i32, _>(0) as i64,
                "title": r.get::<String, _>(1),
                "feed_url": r.get::<String, _>(2),
                "last_error": r.get::<String, _>(3),
                "site_url": r.get::<Option<String>, _>(4).unwrap_or_default(),
            })
        })
        .collect();
    Value::Array(items)
}

/// A `ttrss_feed_categories` row (ordered by `order_id`, `title`).
struct CatRow {
    id: i64,
    title: String,
    parent_cat: Option<i64>,
}

/// A `ttrss_feeds` row + its article count (ordered by `order_id`, `title`).
struct FeedRow {
    id: i64,
    title: String,
    last_error: String,
    update_interval: i64,
    last_updated: Option<String>,
    num_articles: i64,
    cat_id: Option<i64>,
}

/// `Pref_Feeds::inactiveFeeds` — the feeds whose latest article is older than
/// 3 months. The PHP does `foreach ($feeds as $f) { $f['last_article'] = ... }`
/// **by value**, so the `make_local_datetime` reassignment is discarded and the
/// raw `MAX(e.updated)` timestamp string is what gets emitted. We reproduce that
/// faithfully (a `to_char` of the stored UTC timestamp, `YYYY-MM-DD HH24:MI:SS`).
pub async fn inactive_feeds(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let rows = sqlx::query(
        "SELECT f.id, f.title, f.site_url, f.feed_url, \n         to_char(MAX(e.updated), 'YYYY-MM-DD HH24:MI:SS') AS last_article\n         FROM ttrss_feeds AS f\n         JOIN ttrss_user_entries AS ue ON ue.feed_id = f.id\n         JOIN ttrss_entries AS e ON e.id = ue.ref_id\n         WHERE f.owner_uid = $1\n           AND (SELECT MAX(ttrss_entries.updated) FROM ttrss_entries\n                JOIN ttrss_user_entries\n                    ON ttrss_entries.id = ttrss_user_entries.ref_id\n                WHERE ttrss_user_entries.feed_id = f.id)\n                < NOW() - INTERVAL '3 months'\n         GROUP BY f.title, f.id, f.site_url, f.feed_url\n         ORDER BY MAX(e.updated) ASC",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i32, _>(0) as i64,
                "title": r.get::<String, _>(1),
                "site_url": r.get::<Option<String>, _>(2),
                "feed_url": r.get::<String, _>(3),
                "last_article": r.get::<Option<String>, _>(4),
            })
        })
        .collect();
    Value::Array(items)
}

/// `Pref_Feeds::getfeedtree` → `_makefeedtree()`.
pub async fn getfeedtree(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let enable_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &rows, profile).as_bool();
    let fresh_max_age = prefs::resolve(prefs::FRESH_ARTICLE_MAX_AGE, &rows, profile).as_int();
    let tz = TzCtx {
        user_tz: prefs::resolve(prefs::USER_TIMEZONE, &rows, profile)
            .as_str()
            .to_string(),
        client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
        short_date_format: prefs::resolve(prefs::SHORT_DATE_FORMAT, &rows, profile)
            .as_str()
            .to_string(),
        long_date_format: prefs::resolve(prefs::LONG_DATE_FORMAT, &rows, profile)
            .as_str()
            .to_string(),
    };

    let mode_is_2 = is_mode_2(extra);
    let search = if mode_is_2 {
        extra
            .get("search")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        String::new() // `$_SESSION["prefs_feed_search"] ?? ""`
    };
    let force_show_empty = extra
        .get("force_show_empty")
        .map(|v| util::param_to_bool(Some(v)))
        .unwrap_or(false);

    let cats = fetch_all_cats(pool, uid).await;
    let feeds = fetch_all_feeds(pool, uid).await;

    let mut root_items: Vec<Value> = Vec::new();
    let mut root_children = 0i64;

    // ---- Mode 2: special + label "categories" first. ----
    if mode_is_2 {
        // `feedlist_init_cat(CATEGORY_SPECIAL)` + the six virtual feeds.
        let mut special = Map::new();
        special.insert("id".into(), json!(format!("CAT:{CAT_SPECIAL}")));
        special.insert("items".into(), Value::Array(Vec::new()));
        special.insert("name".into(), json!(cat_title(CAT_SPECIAL)));
        special.insert("type".into(), json!("category"));
        special.insert("unread".into(), json!(-1));
        special.insert("bare_id".into(), json!(CAT_SPECIAL));
        for feed_id in [
            FEED_ALL,
            FEED_FRESH,
            FEED_STARRED,
            FEED_PUBLISHED,
            FEED_ARCHIVED,
            FEED_RECENTLY_READ,
        ] {
            special
                .get_mut("items")
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(special_feed(pool, uid, feed_id, fresh_max_age).await);
        }
        // (Plugin feeds for CATEGORY_SPECIAL — none for this deployment.)
        if enable_cats {
            root_items.push(Value::Object(special));
        }

        // Labels category.
        let labels = sqlx::query(
            "SELECT id, fg_color, bg_color, caption FROM ttrss_labels2
             WHERE owner_uid = $1 ORDER BY caption",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        if !labels.is_empty() {
            let mut label_cat: Map<String, Value> = if enable_cats {
                let mut m = Map::new();
                m.insert("id".into(), json!(format!("CAT:{CAT_LABELS}")));
                m.insert("items".into(), Value::Array(Vec::new()));
                m.insert("name".into(), json!(cat_title(CAT_LABELS)));
                m.insert("type".into(), json!("category"));
                m.insert("unread".into(), json!(-1));
                m.insert("bare_id".into(), json!(CAT_LABELS));
                m
            } else {
                let mut m = Map::new();
                m.insert("items".into(), Value::Array(Vec::new()));
                m
            };
            for r in &labels {
                let label_id = r.get::<i32, _>(0) as i64;
                let mut feed = special_feed(
                    pool,
                    uid,
                    crate::labels::label_to_feed_id(label_id),
                    fresh_max_age,
                )
                .await;
                if let Value::Object(obj) = &mut feed {
                    obj.insert("name".into(), json!(r.get::<String, _>(3)));
                    obj.insert("fg_color".into(), json!(r.get::<String, _>(1)));
                    obj.insert("bg_color".into(), json!(r.get::<String, _>(2)));
                }
                label_cat
                    .get_mut("items")
                    .and_then(Value::as_array_mut)
                    .unwrap()
                    .push(feed);
            }
            if enable_cats {
                root_items.push(Value::Object(label_cat));
            }
        }
    }

    // ---- Real categories (and uncategorized) — same for modes 0 and 2. ----
    if enable_cats {
        let show_empty_cats = force_show_empty || (!mode_is_2 && search.is_empty());

        // Top-level categories (`parent_cat IS NULL`), ordered (order_id, title).
        for c in cats.iter().filter(|c| c.parent_cat.is_none()) {
            let items = build_cat_items(&cats, &feeds, state, c.id, show_empty_cats, &tz);
            let n = calculate_children_count(&items);
            let mut cat = Map::new();
            cat.insert("id".into(), json!(format!("CAT:{}", c.id)));
            cat.insert("bare_id".into(), json!(c.id));
            cat.insert("auxcounter".into(), json!(-1));
            cat.insert("name".into(), json!(c.title.clone()));
            cat.insert("items".into(), Value::Array(items));
            cat.insert("checkbox".into(), json!(false));
            cat.insert("type".into(), json!("category"));
            cat.insert("unread".into(), json!(-1));
            cat.insert("child_unread".into(), json!(-1));
            cat.insert("param".into(), json!(nfeeds(n)));
            if n > 0 || show_empty_cats {
                root_children += n;
                root_items.push(Value::Object(cat));
            }
        }

        // Uncategorized (a special case: `cat_id IS NULL`).
        let items: Vec<Value> = feeds
            .iter()
            .filter(|f| f.cat_id.is_none())
            .map(|f| feed_node(state, f, &tz))
            .collect();
        let n = items.len() as i64;
        if n > 0 || show_empty_cats {
            root_children += n;
            let mut unc = Map::new();
            unc.insert("id".into(), json!("CAT:0"));
            unc.insert("bare_id".into(), json!(0i64));
            unc.insert("auxcounter".into(), json!(-1));
            unc.insert("name".into(), json!("Uncategorized"));
            unc.insert("items".into(), Value::Array(items));
            unc.insert("type".into(), json!("category"));
            unc.insert("checkbox".into(), json!(false));
            unc.insert("unread".into(), json!(-1));
            unc.insert("child_unread".into(), json!(-1));
            unc.insert("param".into(), json!(nfeeds(n)));
            root_items.push(Value::Object(unc));
        }
    } else {
        // Cats disabled: every feed directly under the root.
        root_items = feeds.iter().map(|f| feed_node(state, f, &tz)).collect();
        root_children = feeds.len() as i64;
    }

    let items = if mode_is_2 {
        Value::Array(root_items)
    } else {
        let mut root = Map::new();
        root.insert("id".into(), json!("root"));
        root.insert("name".into(), json!("Feeds"));
        root.insert("items".into(), Value::Array(root_items));
        root.insert("param".into(), json!(nfeeds(root_children)));
        root.insert("type".into(), json!("category"));
        Value::Array(vec![Value::Object(root)])
    };

    let mut out = Map::new();
    out.insert("identifier".into(), json!("id"));
    out.insert("label".into(), json!("name"));
    out.insert("items".into(), items);
    Value::Object(out)
}

/// `Pref_Feeds::index` — the feeds settings tab: a `TabContainer` with the
/// "My feeds" toolbar/tree, "OPML" import/export, and "Sharing" panes
/// (`index_feeds` / `index_opml` / `index_shared`).
pub async fn index(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let enable_feed_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &rows, profile).as_bool();

    // `index_feeds`: the feed-tree search term — `$_REQUEST["search"]` when
    // present (also cached in the session), else the cached value.
    let feed_search = if extra.contains_key("search") {
        let s = util::clean_str(extra.get("search").and_then(Value::as_str).unwrap_or(""));
        sess.with_mut(|d| {
            d.set(
                "prefs_feed_search",
                crate::core::phpser::Value::Bytes(s.as_bytes().to_vec()),
            )
        });
        s
    } else {
        sess.data()
            .get_str("prefs_feed_search")
            .unwrap_or("")
            .to_string()
    };
    let auto_expand = if feed_search.is_empty() {
        "false"
    } else {
        "true"
    };

    let mut ctx: Map<String, Value> = Map::new();
    ctx.insert(
        "feed_search".into(),
        Value::String(util::htmlspecialchars(&feed_search)),
    );
    ctx.insert("auto_expand".into(), Value::String(auto_expand.to_string()));
    ctx.insert("enable_feed_cats".into(), Value::Bool(enable_feed_cats));
    ctx.insert(
        "csrf_token".into(),
        Value::String(
            sess.data()
                .get_str("csrf_token")
                .unwrap_or_default()
                .to_string(),
        ),
    );
    ctx.insert(
        "opml_notice".into(),
        Value::String(controls::format_notice(
            "Only main settings profile can be migrated using OPML.",
        )),
    );
    ctx.insert(
        "shared_notice".into(),
        Value::String(controls::format_notice(
            "Published articles can be subscribed by anyone who knows the following URL:",
        )),
    );
    ctx.insert(
        "include_settings_checkbox".into(),
        Value::String(controls::checkbox_tag("include_settings", true, "1", &[])),
    );

    feeds_index_env()
        .get_template("feeds_index")
        .expect("feeds_index template")
        .render(ctx)
        .expect("feeds_index template render")
}

/// The minijinja environment holding `feeds_index` (parsed once).
fn feeds_index_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template(
            "feeds_index",
            include_str!("../templates/feeds_index.html.jinja"),
        )
        .expect("valid feeds_index template");
        env
    })
}

/// `Feeds::_get_cat_title` for the special/uncategorized ids (real category
/// titles come from the DB rows).
fn cat_title(cat_id: i64) -> &'static str {
    match cat_id {
        CAT_SPECIAL => "Special",
        CAT_LABELS => "Labels",
        _ => "Uncategorized",
    }
}

/// A special-feed or label node: `feedlist_init_feed(feed_id)`.
async fn special_feed(pool: &sqlx::PgPool, uid: i64, feed_id: i64, fresh_max_age: i64) -> Value {
    let unread = counters(pool, uid, feed_id, fresh_max_age).await;
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("FEED:{feed_id}")));
    m.insert("name".into(), json!(special_title(feed_id)));
    m.insert("unread".into(), json!(unread));
    m.insert("type".into(), json!("feed"));
    m.insert("error".into(), json!(""));
    m.insert("updated".into(), json!(""));
    m.insert("icon".into(), json!(special_icon(feed_id)));
    m.insert("bare_id".into(), json!(feed_id));
    m.insert("auxcounter".into(), json!(0));
    Value::Object(m)
}

/// `Feeds::_get_title` for the virtual feeds.
fn special_title(feed_id: i64) -> &'static str {
    match feed_id {
        FEED_STARRED => "Starred articles",
        FEED_PUBLISHED => "Published articles",
        FEED_FRESH => "Fresh articles",
        FEED_ALL => "All articles",
        FEED_ARCHIVED => "Archived articles",
        FEED_RECENTLY_READ => "Recently read",
        _ => "",
    }
}

/// `Feeds::_get_title($id, $owner_uid)` (the non-cat branch): virtual feeds,
/// labels (with the `Unknown label (n)` fallback) and real feeds.
pub async fn feed_title(pool: &sqlx::PgPool, id: i64, owner_uid: i64) -> String {
    match id {
        FEED_STARRED => "Starred articles".to_string(),
        FEED_PUBLISHED => "Published articles".to_string(),
        FEED_FRESH => "Fresh articles".to_string(),
        FEED_ALL => "All articles".to_string(),
        FEED_ARCHIVED => "Archived articles".to_string(),
        FEED_RECENTLY_READ => "Recently read".to_string(),
        _ if id < LABEL_BASE_INDEX => {
            let label_id = crate::labels::feed_to_label_id(id);
            sqlx::query_scalar::<_, String>(
                "SELECT caption FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2 LIMIT 1",
            )
            .bind(label_id as i32)
            .bind(owner_uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("Unknown label ({})", label_id))
        }
        _ if id > 0 => sqlx::query_scalar::<_, String>(
            "SELECT title FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2 LIMIT 1",
        )
        .bind(id as i32)
        .bind(owner_uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("Unknown feed ({})", id)),
        _ => id.to_string(),
    }
}

/// `Feeds::_get_purge_interval($feed_id)` — the feed's own `purge_interval`
/// (days), or the owner's `PURGE_OLD_DAYS` pref (default 60) when the feed
/// uses the default (`0`). Returns `-1` when the feed does not exist (the
/// caller treats it as "do not purge").
async fn feed_purge_interval(pool: &sqlx::PgPool, feed_id: i32) -> i32 {
    let row = sqlx::query("SELECT purge_interval, owner_uid FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(r) = row else { return -1 };
    let feed_purge: i32 = r.get(0);
    let owner_uid: i32 = r.get(1);
    if feed_purge != 0 {
        return feed_purge;
    }
    // `Prefs::get(Prefs::PURGE_OLD_DAYS, owner_uid)` (the default profile).
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 WHERE owner_uid = $1 AND profile IS NULL \
         AND pref_name = 'PURGE_OLD_DAYS'",
    )
    .bind(owner_uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let rows: Vec<(String, Option<i32>, String)> = match value {
        Some(v) => vec![(prefs::PURGE_OLD_DAYS.to_string(), None, v)],
        None => Vec::new(),
    };
    prefs::resolve(prefs::PURGE_OLD_DAYS, &rows, None).as_int() as i32
}

/// `Feeds::_purge($feed_id, $purge_interval)` — delete the feed's old entries
/// (`ttrss_user_entries`): never marked ones, and unread ones only when
/// `PURGE_UNREAD_ARTICLES` is set (or `FORCE_ARTICLE_PURGE` overrides the
/// whole purge: interval = `FORCE_ARTICLE_PURGE`, `purge_unread = true`).
///
/// `purge_interval = 0` means "resolve it" (the feed's own interval or the
/// owner's `PURGE_OLD_DAYS` pref). Returns the number of deleted rows, or
/// `None` when the feed does not exist or purging is disabled
/// (`purge_interval <= 0`).
pub async fn purge_feed(
    pool: &sqlx::PgPool,
    cfg: &crate::core::config::Config,
    feed_id: i32,
    purge_interval: i32,
) -> Option<i64> {
    let purge_interval = if purge_interval == 0 {
        feed_purge_interval(pool, feed_id).await
    } else {
        purge_interval
    };

    let owner_uid: Option<i32> = sqlx::query_scalar("SELECT owner_uid FROM ttrss_feeds WHERE id = $1")
        .bind(feed_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let owner_uid = owner_uid?;

    let (purge_unread, purge_interval) = if cfg.force_article_purge != 0 {
        (true, cfg.force_article_purge)
    } else {
        let value: Option<String> = sqlx::query_scalar(
            "SELECT value FROM ttrss_user_prefs2 WHERE owner_uid = $1 AND profile IS NULL \
             AND pref_name = 'PURGE_UNREAD_ARTICLES'",
        )
        .bind(owner_uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        let rows: Vec<(String, Option<i32>, String)> = match value {
            Some(v) => vec![(prefs::PURGE_UNREAD_ARTICLES.to_string(), None, v)],
            None => Vec::new(),
        };
        (
            prefs::resolve(prefs::PURGE_UNREAD_ARTICLES, &rows, None).as_bool(),
            purge_interval,
        )
    };

    let purge_interval = purge_interval as i64;
    if purge_interval <= 0 {
        return None;
    }

    let extra = if purge_unread { "" } else { "unread = false AND " };
    let sql = format!(
        "DELETE FROM ttrss_user_entries USING ttrss_entries WHERE ttrss_entries.id = ref_id \
         AND marked = false AND feed_id = $1 AND {extra} \
         ttrss_entries.date_updated < NOW() - INTERVAL '{purge_interval} days'"
    );
    let res = sqlx::query(AssertSqlSafe(sql)).bind(feed_id).execute(pool).await.ok()?;
    Some(res.rows_affected() as i64)
}

/// `Feeds::_catchup`: mark the owner's unread entries as read
/// (`last_read = NOW()`) within the scope of a category / feed / virtual
/// feed / label / tag name. `mode` is the optional recency window
/// (`1day`/`1week`/`2week`); `search`/`search_lang` optionally restrict the
/// scope to the search results (plugin `HOOK_SEARCH` is M7 territory — the
/// `_search_to_sql` fallback is used directly).
#[allow(clippy::too_many_arguments)]
pub async fn catchup_scope(
    pool: &sqlx::PgPool,
    owner_uid: i64,
    profile: Option<i32>,
    rows: &[(String, Option<i32>, String)],
    feed_id_or_tag: &str,
    is_cat: bool,
    mode: &str,
    search: &str,
    search_lang: &str,
) {
    let date_qpart = match mode {
        "1day" | "1week" | "2week" => {
            let n = mode.chars().next().unwrap();
            let unit = &mode[1..];
            format!("date_entered < NOW() - INTERVAL '{n} {unit}'")
        }
        _ => "true".to_string(),
    };
    // PHP: `is_array($search) && $search[0]` → the `_search_to_sql` fragment
    // (no dry-run precheck here — a fragment that errors at execution is
    // swallowed by the `let _ = …` below, cf. the uncaught PDOException the
    // PHP would throw); otherwise `true`.
    let search_qpart = if search.is_empty() {
        "true".to_string()
    } else {
        crate::search::search_to_sql_cached(pool, rows, owner_uid, profile, search, search_lang)
            .await
            .query_part
    };

    if !crate::util::is_numeric(feed_id_or_tag) {
        // tag
        let _ = sqlx::query(AssertSqlSafe(format!(
            "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
             WHERE ref_id IN (SELECT id FROM
                 (SELECT DISTINCT ttrss_entries.id FROM ttrss_entries, ttrss_user_entries, ttrss_tags
                  WHERE ref_id = ttrss_entries.id AND post_int_id = int_id AND tag_name = $1
                  AND ttrss_user_entries.owner_uid = $2 AND unread = true
                  AND {date_qpart} AND {search_qpart}) as tmp)"
        )))
        .bind(feed_id_or_tag)
        .bind(owner_uid)
        .execute(pool)
        .await;
        return;
    }

    let feed_id = crate::util::php_int_cast(feed_id_or_tag);
    if is_cat {
        if feed_id >= 0 {
            let cat_qpart = if feed_id == 0 {
                // `Feeds::CATEGORY_UNCATEGORIZED`
                "cat_id IS NULL".to_string()
            } else {
                let mut children = crate::headlines::child_cats(pool, owner_uid, feed_id).await;
                children.push(feed_id);
                format!(
                    "cat_id IN ({})",
                    children
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            let _ = sqlx::query(AssertSqlSafe(format!(
                "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                 WHERE ref_id IN (SELECT id FROM
                     (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                      WHERE ref_id = id AND owner_uid = $1 AND unread = true
                      AND feed_id IN (SELECT id FROM ttrss_feeds WHERE {cat_qpart})
                      AND {date_qpart} AND {search_qpart}) as tmp)"
            )))
            .bind(owner_uid)
            .execute(pool)
            .await;
        } else if feed_id == CAT_LABELS {
            let _ = sqlx::query(AssertSqlSafe(format!(
                "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                 WHERE (SELECT COUNT(*) FROM ttrss_user_labels2, ttrss_entries
                        WHERE article_id = ref_id AND id = ref_id
                        AND {date_qpart} AND {search_qpart}) > 0
                 AND unread = true AND owner_uid = $1"
            )))
            .bind(owner_uid)
            .execute(pool)
            .await;
        }
    } else if feed_id > 0 {
        let _ = sqlx::query(AssertSqlSafe(format!(
            "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
             WHERE ref_id IN (SELECT id FROM
                 (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                  WHERE ref_id = id AND owner_uid = $1 AND unread = true AND feed_id = $2
                  AND {date_qpart} AND {search_qpart}) as tmp)"
        )))
        .bind(owner_uid)
        .bind(feed_id)
        .execute(pool)
        .await;
    } else if feed_id < 0 && feed_id > LABEL_BASE_INDEX {
        // special (virtual) feeds
        let query = match feed_id {
            FEED_STARRED => Some(format!(
                "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                 WHERE ref_id IN (SELECT id FROM
                     (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                      WHERE ref_id = id AND owner_uid = $1 AND unread = true AND marked = true
                      AND {date_qpart} AND {search_qpart}) as tmp)"
            )),
            FEED_PUBLISHED => Some(format!(
                "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                 WHERE ref_id IN (SELECT id FROM
                     (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                      WHERE ref_id = id AND owner_uid = $1 AND unread = true AND published = true
                      AND {date_qpart} AND {search_qpart}) as tmp)"
            )),
            FEED_FRESH => {
                let intl = prefs::resolve(prefs::FRESH_ARTICLE_MAX_AGE, rows, profile).as_int();
                Some(format!(
                    "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                     WHERE ref_id IN (SELECT id FROM
                         (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                          WHERE ref_id = id AND owner_uid = $1 AND score >= 0 AND unread = true
                          AND date_entered > NOW() - INTERVAL '{intl} hour'
                          AND {date_qpart} AND {search_qpart}) as tmp)"
                ))
            }
            FEED_ALL => Some(format!(
                "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
                 WHERE ref_id IN (SELECT id FROM
                     (SELECT DISTINCT id FROM ttrss_entries, ttrss_user_entries
                      WHERE ref_id = id AND owner_uid = $1 AND unread = true
                      AND {date_qpart} AND {search_qpart}) as tmp)"
            )),
            _ => None,
        };
        if let Some(q) = query {
            let _ = sqlx::query(AssertSqlSafe(q)).bind(owner_uid).execute(pool).await;
        }
    } else if feed_id < LABEL_BASE_INDEX {
        // label
        let label_id = crate::labels::feed_to_label_id(feed_id);
        let _ = sqlx::query(AssertSqlSafe(format!(
            "UPDATE ttrss_user_entries SET unread = false, last_read = NOW()
             WHERE ref_id IN (SELECT id FROM
                 (SELECT DISTINCT ttrss_entries.id FROM ttrss_entries, ttrss_user_entries, ttrss_user_labels2
                  WHERE ref_id = id AND label_id = $1 AND ref_id = article_id
                  AND owner_uid = $2 AND unread = true
                  AND {date_qpart} AND {search_qpart}) as tmp)"
        )))
        .bind(label_id)
        .bind(owner_uid)
        .execute(pool)
        .await;
    }
}

/// `RPC::catchupFeed` — mark the feed / category / label / tag read, then
/// return the full counters synchronously (the frontend uses them to find
/// the next unread feed).
pub async fn catchup_feed(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    // PHP: `clean(...)` = `strip_tags` + trim — use `clean_str` (not a bare
    // `.trim()`) so these match PHP exactly, including tag stripping.
    let clean_param =
        |k: &str| util::clean_str(extra.get(k).and_then(|v| v.as_str()).unwrap_or(""));
    let feed_id = clean_param("feed_id");
    let is_cat = extra
        .get("is_cat")
        .is_some_and(|v| util::param_to_bool(Some(v)));
    let mode = clean_param("mode");
    let search_query = clean_param("search_query");
    let search_lang = clean_param("search_lang");
    catchup_scope(
        pool,
        uid,
        profile,
        &rows,
        &feed_id,
        is_cat,
        &mode,
        &search_query,
        &search_lang,
    )
    .await;
    let client_tz_offset = data.get_int("clientTzOffset").unwrap_or(0);
    let counters = crate::counters::all_counters(
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
    .await;
    json!({ "counters": counters })
}

/// `Feeds::catchupAll` — mark all of the session user's entries read.
pub async fn catchup_all(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let _ = sqlx::query(
        "UPDATE ttrss_user_entries SET last_read = NOW(), unread = false
         WHERE unread = true AND owner_uid = $1",
    )
    .bind(uid)
    .execute(state.db.pool())
    .await;
    json!({ "message": "UPDATE_COUNTERS" })
}

/// `Feeds::opensite` — 302-redirect to a feed's site URL (validated), or 404 if
/// the feed is missing / has no valid site URL. Not a JSON method: it emits a
/// raw `Location` header (302) or a 404 body.
pub async fn opensite(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> (StatusCode, Option<String>, String) {
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = crate::util::clean_int(extra.get("feed_id")) as i32;

    let site_url: Option<String> =
        sqlx::query_scalar("SELECT site_url FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2")
            .bind(feed_id)
            .bind(uid)
            .fetch_optional(state.db.pool())
            .await
            .ok()
            .flatten();

    if let Some(url) = site_url {
        if let Some(valid) = crate::urlhelper::validate(&url, false) {
            return (StatusCode::FOUND, Some(valid), String::new());
        }
    }

    (
        StatusCode::NOT_FOUND,
        None,
        "Feed not found or has an empty site URL.".into(),
    )
}
/// `Feeds::subscribeToFeed` — the data for the "Add a feed" dialog: the
/// category `<select>` (`cat`) and the update-interval options (the default-
/// annotated `$update_intervals`). Pure read. PHP emits `intervals.update` as a
/// JSON object; the frontend's `select_hash` re-sorts the integer-like keys
/// numerically, so the object key order is functionally irrelevant here.
pub async fn subscribe_to_feed(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let pref_rows = prefs::load_rows(&state.db, uid, None).await;
    let default_update = prefs::resolve(prefs::DEFAULT_UPDATE_INTERVAL, &pref_rows, None).as_int();

    let cat_select = select_feeds_cats(pool, uid, "cat", None, "").await;

    json!({
        "cat_select": cat_select,
        "intervals": {
            "update": update_intervals_with_default(default_update),
        },
    })
}

/// `Pref_Feeds::batchSubscribe` — the data the batch-subscribe dialog needs:
/// the `ENABLE_FEED_CATS` pref (as an int, `1`/`0`, per the PHP `(int)` cast) +
/// the category `<select>`.
pub async fn batch_subscribe(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let pool = state.db.pool();
    let pref_rows = prefs::load_rows(&state.db, uid, None).await;
    let enable_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &pref_rows, profile).as_bool();
    let cat_select = select_feeds_cats(pool, uid, "cat", None, "").await;
    json!({
        "enable_cats": enable_cats as i64,
        "cat_select": cat_select,
    })
}

/// `Pref_Feeds::batchAddFeeds` — insert the posted feed URLs (one per line) as
/// `[Unknown]` feeds for the user, skipping any the user already subscribes to
/// and any that fail `UrlHelper::validate`. Read-only users are a no-op (the
/// PHP early-returns with no error code). This is a pure DB insert: the new
/// feeds are picked up by the daemon on its next update (it does *not* fetch +
/// parse now — that is the M6 `Feeds::add` engine).
pub async fn batch_add_feeds(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();

    // `access_level == ACCESS_LEVEL_READONLY` → no-op (the PHP early-returns).
    if sess.data().access_level().unwrap_or(0) == crate::core::user::ACCESS_LEVEL_READONLY as i64 {
        return Value::Null;
    }

    let cat_id = util::clean_int(extra.get("cat")) as i32;
    // `$cat_id ?: null` — an empty/`0` category means uncategorized (NULL).
    let cat_id_sql: Option<i32> = (cat_id != 0).then_some(cat_id);
    let feeds_raw = util::clean_str(extra.get("feeds").and_then(|v| v.as_str()).unwrap_or(""));
    let login = util::clean_str(extra.get("login").and_then(|v| v.as_str()).unwrap_or(""));
    let pass = util::clean_str(extra.get("pass").and_then(|v| v.as_str()).unwrap_or(""));

    for feed in feeds_raw.split('\n') {
        let feed = feed.trim();
        let Some(valid) = crate::urlhelper::validate(feed, false) else {
            continue;
        };
        if let Ok(mut tx) = pool.begin().await {
            // Skip feeds this user already has (matched on the validated URL).
            let existing: Option<i32> = sqlx::query_scalar(
                "SELECT id FROM ttrss_feeds WHERE feed_url = $1 AND owner_uid = $2",
            )
            .bind(&valid)
            .bind(uid)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
            if existing.is_none() {
                let _ = sqlx::query(
                    "INSERT INTO ttrss_feeds
                        (owner_uid, feed_url, title, cat_id, auth_login, auth_pass, update_method, auth_pass_encrypted)
                     VALUES ($1, $2, '[Unknown]', $3, $4, $5, 0, false)",
                )
                .bind(uid)
                .bind(&valid)
                .bind(cat_id_sql)
                .bind(&login)
                .bind(&pass)
                .execute(&mut *tx)
                .await;
            }
            let _ = tx.commit().await;
        }
    }

    Value::Null
}

/// `Feeds::_is_html($content)` — true if the first 8192 bytes contain `<html`
/// or `DOCTYPE html` (case-insensitive).
pub fn is_html(content: &str) -> bool {
    let head = &content[..content.len().min(8192)];
    let lower = head.to_ascii_lowercase();
    lower.contains("<html") || lower.contains("doctype html")
}

/// `Feeds::_get_feeds_from_html($url, $content)` — discover the feed `<link>`
/// elements in an HTML page. For each `<link rel="alternate">` with an `rss` /
/// `atom` type, or `<link rel="feed">`, in the `<head>` or `<body>`, yields a
/// `(feed_url, title)` pair (the title is the `title` attribute, falling back
/// to the `type` attribute). Deduped by feed URL; the base URL is the page
/// URL up to and including its last `/`.
pub fn get_feeds_from_html(url: &str, content: &str) -> Vec<(String, String)> {
    let url = match crate::urlhelper::validate(url, false) {
        Some(u) => u,
        None => return Vec::new(),
    };
    let base_url = match url.rfind('/') {
        Some(i) => url[..=i].to_string(),
        None => url.clone(),
    };

    let doc = parse_html().one(content);
    let Ok(select_iter) = doc.select("html head link, html body link") else {
        return Vec::new();
    };

    let mut feed_urls: Vec<(String, String)> = Vec::new();
    for link in select_iter {
        let attrs = link.attributes.borrow();
        let href = attrs.get("href").unwrap_or("").trim().to_string();
        let rel = attrs.get("rel").unwrap_or("").to_string();
        let type_ = attrs.get("type").unwrap_or("").to_string();
        let title = attrs.get("title").unwrap_or("").to_string();
        drop(attrs);

        if href.is_empty() {
            continue;
        }
        let is_alt_feed =
            rel == "alternate" && (type_.contains("rss") || type_.contains("atom"));
        if !is_alt_feed && rel != "feed" {
            continue;
        }
        let title = if title.trim().is_empty() { type_ } else { title };
        let Some(feed_url) = crate::urlhelper::rewrite_relative(&base_url, &href) else {
            continue;
        };
        if !feed_urls.iter().any(|(u, _)| u == &feed_url) {
            feed_urls.push((feed_url, title));
        }
    }
    feed_urls
}

/// `Feeds::_subscribe($url, $cat_id, $auth_login, $auth_pass, $update_interval)`
/// — the shared subscribe engine behind `Feeds::add` (the RPC) and
/// `API::subscribeToFeed`. Validates + fetches the URL, discovers the feed from
/// an HTML page if needed, parses it, and inserts the feed (title `[Unknown]`)
/// — then runs the immediate first update (`RSSUtils::update_basic_info`).
/// Returns the PHP's `{code, …}` array. The `HOOK_PRE_SUBSCRIBE` /
/// `HOOK_SUBSCRIBE_FEED` plugin hooks (M8) are not ported.
#[allow(clippy::too_many_arguments)] // mirrors `Feeds::_subscribe`'s parameter list
pub async fn subscribe(
    cfg: &crate::core::config::Config,
    pool: &sqlx::PgPool,
    uid: i64,
    access_level: i32,
    url: &str,
    cat_id: i32,
    auth_login: &str,
    auth_pass: &str,
    update_interval: i32,
) -> Value {
    // `if ($user && $user->access_level == ACCESS_LEVEL_READONLY) return ["code" => 8]`.
    if access_level == crate::core::user::ACCESS_LEVEL_READONLY {
        return json!({ "code": 8 });
    }

    // `$url = UrlHelper::validate($url); if (!$url) return ["code" => 2]`.
    let Some(url) = crate::urlhelper::validate(url, false) else {
        return json!({ "code": 2 });
    };

    // `$contents = UrlHelper::fetch(['url', 'login', 'pass'])` (default timeout).
    let res = crate::urlhelper::fetch(&crate::urlhelper::FetchOptions {
        url: url.clone(),
        login: if auth_login.trim().is_empty() {
            None
        } else {
            Some(auth_login.to_string())
        },
        pass: if auth_pass.trim().is_empty() {
            None
        } else {
            Some(auth_pass.to_string())
        },
        filtering: Some(cfg.fetch_extended_url_filtering),
        ..Default::default()
    })
    .await;

    let contents = res
        .body
        .as_deref()
        .map_or(String::new(), |b| String::from_utf8_lossy(b).into_owned());

    // `if (empty($contents)) return ["code" => 5, "message" => …]`.
    if contents.is_empty() {
        return json!({
            "code": 5,
            "message": util::truncate_string(&util::clean_str(&res.error), 250, "…"),
        });
    }

    // `if (str_contains($fetch_last_content_type, "html") && _is_html($contents))`
    // — an HTML page: discover the feed link(s).
    let mut url = url;
    if res.content_type.to_lowercase().contains("html") && is_html(&contents) {
        let feed_urls = get_feeds_from_html(&url, &contents);
        if feed_urls.is_empty() {
            let message = if access_level >= crate::core::user::ACCESS_LEVEL_ADMIN {
                util::truncate_string(&util::clean_str(&contents), 250, "…")
            } else {
                String::new()
            };
            return json!({ "code": 3, "message": message });
        } else if feed_urls.len() > 1 {
            let feeds: Vec<String> = feed_urls.iter().map(|(u, _)| u.clone()).collect();
            return json!({ "code": 4, "feeds": feeds });
        }
        url = feed_urls.into_iter().next().expect("non-empty").0;
    }

    // `Don't allow subscribing if the content is invalid`.
    let mut parser = crate::core::feed::FeedParser::new(&contents);
    let parse_error = parser.error();
    if !parser.init() {
        let message = if !parse_error.is_empty() {
            util::truncate_string(&util::clean_str(&parse_error), 250, "…")
        } else if access_level >= crate::core::user::ACCESS_LEVEL_ADMIN {
            util::truncate_string(&util::clean_str(&contents), 250, "…")
        } else {
            String::new()
        };
        return json!({ "code": 6, "message": message });
    }

    // The feed already exists (matched on the validated URL + owner)?
    let existing: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE feed_url = $1 AND owner_uid = $2")
            .bind(&url)
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if let Some(id) = existing {
        return json!({ "code": 0, "feed_id": id });
    }

    // Create the feed (title `[Unknown]`, `auth_pass_encrypted = false`).
    let cat_id_sql: Option<i32> = (cat_id != 0).then_some(cat_id);
    let new_id: Result<i32, _> = sqlx::query_scalar(
        "INSERT INTO ttrss_feeds \
         (owner_uid, feed_url, title, cat_id, auth_login, auth_pass, update_method, \
          update_interval, auth_pass_encrypted) \
         VALUES ($1, $2, '[Unknown]', $3, $4, $5, 0, $6, false) RETURNING id",
    )
    .bind(uid)
    .bind(&url)
    .bind(cat_id_sql)
    .bind(auth_login)
    .bind(auth_pass)
    .bind(update_interval)
    .fetch_one(pool)
    .await;

    match new_id {
        Ok(id) => {
            crate::rss_utils::update_basic_info(pool, cfg, id).await;
            json!({ "code": 1, "feed_id": id })
        }
        Err(_) => json!({ "code": 7 }),
    }
}

/// `Feeds::add` — the subscribe action (the read-side dialog's "Add feed").
/// Delegates to [`subscribe`] and wraps the result as `{"result": <rc>}`.
pub async fn add(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let access_level = sess.data().access_level().unwrap_or(0) as i32;
    let pool = state.db.pool();

    let feed = util::clean_str(extra.get("feed").and_then(|v| v.as_str()).unwrap_or(""));
    let cat = util::clean_int(extra.get("cat")) as i32;
    let need_auth = extra.get("need_auth").is_some();
    let get_opt = |k: &str| util::clean_str(extra.get(k).and_then(|v| v.as_str()).unwrap_or(""));
    let login = if need_auth { get_opt("login") } else { String::new() };
    let pass = if need_auth { get_opt("pass") } else { String::new() };
    let update_interval = util::clean_int(extra.get("update_interval")) as i32;

    let rc =
        subscribe(&state.config, pool, uid, access_level, &feed, cat, &login, &pass, update_interval)
            .await;
    json!({ "result": rc })
}
/// `Feeds::updatedebugger` — the standalone feed-update debug page
/// (`Content-Type: text/html`). Reproduced verbatim from the PHP template;
/// the cache-bust timestamp and the csrf token normalize away in the diff.
const UPDATE_DEBUGGER_TEMPLATE: &str = r#"		<!DOCTYPE html>
		<html>
		<head>
			<title>Feed Debugger</title>
			<style type='text/css'>
				@media (prefers-color-scheme: dark) {
					body {
						background : #222;
					}
				}
				body.css_loading * {
					display : none;
				}

				.feed-xml {
					color : green;
				}

				.log-timestamp {
					color : gray;
				}

				.log-timestamp::before {
					content: "["
				}

				.log-timestamp::after {
					content: "]"
				}

			</style>
			<script>
				dojoConfig = {
					async: true,
					cacheBust: "1788113929",
					packages: [
						{ name: "fox", location: "../../js" },
					]
				};
			</script>
			<script src="js/utility.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="js/common.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="lib/dojo/dojo.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="lib/dojo/tt-rss-layer.js?1788113929" type="text/javascript" charset="utf-8"></script>
					</head>
		<body class="flat ttrss_utility feed_debugger css_loading">
		<script type="text/javascript">
			require(['dojo/parser', "dojo/ready", 'dijit/form/Button','dijit/form/CheckBox', 'fox/form/Select', 'dijit/form/Form',
				'dijit/form/Select','dijit/form/TextBox','dijit/form/ValidationTextBox'],function(parser, ready){
				ready(function() {
					parser.parse();
				});
			});
		</script>

			<div class="container">
				<h1>Feed Debugger: @FEEDID@: @TITLE@</h1>
				<div class="content">
					<form method="post" action="" dojoType="dijit.form.Form">
						<input dojoType="dijit.form.TextBox" style="display : none"
                name="op"
               value="Feeds">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="method"
               value="updatedebugger">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="csrf_token"
               value="@CSRF@">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="action"
               value="do_update">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="feed_id"
               value="@FEEDID@">
						<fieldset>
							<label>
				<select dojoType='fox.form.Select' name="xdebug" id="" name="xdebug" ><option value="1" @SEL_VERBOSE@>LOG_VERBOSE</option><option value="2" @SEL_EXTENDED@>LOG_EXTENDED</option></select></label>
						</fieldset>

						<fieldset>
				<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="force_refetch" @CHECKED_REFETCH@id=""> Force refetch</label>
						</fieldset>

						<fieldset class="narrow">
				<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="force_rehash" @CHECKED_REHASH@id=""> Force rehash</label>
						</fieldset>

						<fieldset class="narrow">
				<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="dump_feed_xml" @CHECKED_DUMP@id=""> Dump feed XML</label>
						</fieldset>

						<button dojoType="dijit.form.Button" class="alt-primary" type="submit">Continue</button>					</form>

					<hr>

				<pre>@LOG@</pre>
				</div>
			</div>
		</body>
		</html>
		"#;

/// `Feeds::updatedebugger` — the standalone feed-update debug page.
pub async fn updatedebugger(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
    csrf: &str,
) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = crate::util::clean_int(extra.get("feed_id"));

    // The PHP's ownership gate: the feed must exist and be owned by the user.
    let owned: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2")
            .bind(feed_id as i32)
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if owned.is_none() {
        return "Access denied.".to_string();
    }

    // The form's `xdebug` select (the PHP's `Debug::map_loglevel` gate: only
    // `-1, 0, 1, 2` are valid; a missing select — the initial GET — is
    // `LOG_VERBOSE`).
    let xdebug = match extra.get("xdebug") {
        Some(v) => match crate::util::clean_int(Some(v)) {
            v if (-1..=2).contains(&v) => v,
            _ => crate::updates::DEBUG_LEVEL_VERBOSE as i64,
        },
        None => crate::updates::DEBUG_LEVEL_VERBOSE as i64,
    };
    let flags = crate::updates::FeedUpdateFlags {
        force_refetch: extra.contains_key("force_refetch"),
        force_rehash: extra.contains_key("force_rehash"),
        dump_feed_xml: extra.contains_key("dump_feed_xml"),
        no_cache: true, // the PHP's `update_rss_feed($feed_id, true, true)`
    };

    // `action=do_update` (the form's hidden field): run the update, capturing
    // its `Debug::log` output for the `<pre>` below.
    let mut log = crate::updates::FeedDebugLog::default();
    if extra.get("action").and_then(Value::as_str) == Some("do_update") {
        let _ = crate::updates::fetch_one_feed(
            pool,
            &state.config,
            state.plugins.as_ref(),
            feed_id as i32,
            &flags,
            Some(&mut log),
        )
        .await;
    }

    let title = feed_title(pool, feed_id, uid).await;

    UPDATE_DEBUGGER_TEMPLATE
        .replace("@CSRF@", csrf)
        .replace("@FEEDID@", &feed_id.to_string())
        .replace("@TITLE@", &title)
        .replace("@LOG@", &render_debug_log(&log, xdebug))
        .replace(
            "@SEL_VERBOSE@",
            if xdebug == 1 { "selected=\"selected\"" } else { "" },
        )
        .replace(
            "@SEL_EXTENDED@",
            if xdebug == 2 { "selected=\"selected\"" } else { "" },
        )
        .replace(
            "@CHECKED_REFETCH@",
            if flags.force_refetch { "checked" } else { "" },
        )
        .replace(
            "@CHECKED_REHASH@",
            if flags.force_rehash { "checked" } else { "" },
        )
        .replace(
            "@CHECKED_DUMP@",
            if flags.dump_feed_xml { "checked" } else { "" },
        )
}

/// Render the captured update log for the debugger page's `<pre>` — the PHP's
/// `Debug::enable_html(true)` output: each line is
/// `<span class='log-timestamp'>H:i:s/PID</span> <span class='log-message'>…</span>`
/// (timestamp html-escaped by the CSS into `[…]`), `<hr/>` for separators,
/// `<code class='feed-xml'>…</code>` for XML dumps. Only lines whose level is
/// ≤ the selected one are shown (`Debug::$loglevel` gate); the XML dump is
/// gated by its own `dump_feed_xml` flag, not the level.
fn render_debug_log(log: &crate::updates::FeedDebugLog, xdebug: i64) -> String {
    use crate::updates::{
        DebugEntry, DEBUG_SEPARATOR,
    };
    let ts = format!(
        "{}/{}",
        chrono::Local::now().format("%H:%M:%S"),
        std::process::id()
    );
    let esc = |s: &String| crate::util::htmlspecialchars(s);
    let mut out = String::new();
    for entry in &log.entries {
        match entry {
            DebugEntry::Xml(xml) => {
                out.push_str(&format!(
                    "<code class='feed-xml'>{}</code>\n",
                    esc(xml)
                ));
            }
            DebugEntry::Log { level, message } => {
                // The PHP gate: `self::$loglevel < $level` → skip.
                if xdebug < *level as i64 {
                    continue;
                }
                if message == DEBUG_SEPARATOR {
                    out.push_str("<hr/>\n");
                } else {
                    out.push_str(&format!(
                        "<span class='log-timestamp'>{ts}</span> <span class='log-message'>{}</span>\n",
                        esc(message)
                    ));
                }
            }
        }
    }
    out
}

/// `Feeds::_get_cat_title($cat_id, $owner_uid)`.
pub async fn category_title(pool: &sqlx::PgPool, cat_id: i64, owner_uid: i64) -> String {
    match cat_id {
        0 => "Uncategorized".to_string(),
        -1 => "Special".to_string(),
        -2 => "Labels".to_string(),
        _ => sqlx::query_scalar::<_, String>(
            "SELECT title FROM ttrss_feed_categories \n             WHERE id = $1 AND owner_uid = $2 LIMIT 1",
        )
        .bind(cat_id as i32)
        .bind(owner_uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "UNKNOWN".to_string()),
    }
}
/// `Feeds::_get_icon` for the virtual feeds.
fn special_icon(feed_id: i64) -> &'static str {
    match feed_id {
        FEED_ARCHIVED => "archive",
        FEED_STARRED => "star",
        FEED_PUBLISHED => "rss_feed",
        FEED_FRESH => "whatshot",
        FEED_ALL => "inbox",
        FEED_RECENTLY_READ => "restore",
        _ if feed_id < LABEL_BASE_INDEX => "label",
        _ => "",
    }
}

/// `Pref_Feeds::get_category_items(cat_id)` — recursive: nested subcategories
/// (with `parent_id`) then the feeds in this category, both already ordered
/// `(order_id, title)` by the fetch. Empty subcategories are dropped unless
/// `show_empty_cats` (mode 0 / `force_show_empty`).
fn build_cat_items(
    cats: &[CatRow],
    feeds: &[FeedRow],
    state: &AppState,
    cat_id: i64,
    show_empty_cats: bool,
    tz: &TzCtx,
) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();

    for s in cats.iter().filter(|c| c.parent_cat == Some(cat_id)) {
        let sub_items = build_cat_items(cats, feeds, state, s.id, show_empty_cats, tz);
        let n = calculate_children_count(&sub_items);
        let mut cat = Map::new();
        cat.insert("id".into(), json!(format!("CAT:{}", s.id)));
        cat.insert("bare_id".into(), json!(s.id));
        cat.insert("name".into(), json!(s.title.clone()));
        cat.insert("items".into(), Value::Array(sub_items));
        cat.insert("checkbox".into(), json!(false));
        cat.insert("type".into(), json!("category"));
        cat.insert("unread".into(), json!(-1));
        cat.insert("child_unread".into(), json!(-1));
        cat.insert("auxcounter".into(), json!(-1));
        cat.insert("parent_id".into(), json!(cat_id));
        cat.insert("param".into(), json!(nfeeds(n)));
        if n > 0 || show_empty_cats {
            items.push(Value::Object(cat));
        }
    }

    for f in feeds.iter().filter(|f| f.cat_id == Some(cat_id)) {
        items.push(feed_node(state, f, tz));
    }

    items
}

/// A regular feed node (category feed, uncategorized, or flat list).
fn feed_node(state: &AppState, f: &FeedRow, tz: &TzCtx) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(format!("FEED:{}", f.id)));
    out.insert("bare_id".into(), json!(f.id));
    out.insert("auxcounter".into(), json!(-1));
    out.insert("name".into(), json!(f.title.clone()));
    out.insert("checkbox".into(), json!(false));
    out.insert("unread".into(), json!(-1));
    out.insert("error".into(), json!(f.last_error.clone()));
    out.insert("icon".into(), json!(feed_icon_url(state, f.id)));
    out.insert(
        "param".into(),
        json!(articles_param(
            f.num_articles,
            &crate::time::make_local_datetime(f.last_updated.as_deref(), false, tz)
        )),
    );
    out.insert(
        "updates_disabled".into(),
        json!((f.update_interval < 0) as i64),
    );
    Value::Object(out)
}

/// `Feeds::_get_icon_url(feed_id)` — the `public.php?op=feed_icon` URL if the
/// favicon exists, else `""`.
fn feed_icon_url(state: &AppState, feed_id: i64) -> String {
    let dir = &state.config.feed_icon_dir;
    if dir.join(feed_id.to_string()).is_file() {
        format!(
            "{}/public.php?op=feed_icon&id={feed_id}",
            state.config.self_url_rtrim()
        )
    } else {
        String::new()
    }
}

/// `calculate_children_count` — recursive count of feed (non-category) nodes.
fn calculate_children_count(items: &[Value]) -> i64 {
    let mut c = 0i64;
    for child in items {
        if child.get("type").and_then(Value::as_str) == Some("category") {
            if let Some(sub) = child.get("items").and_then(Value::as_array) {
                c += calculate_children_count(sub);
            }
        } else {
            c += 1;
        }
    }
    c
}

/// `_ngettext("(%d article / %s)", "(%d articles / %s)", n)` + the datetime.
fn articles_param(n: i64, hm: &str) -> String {
    let unit = if n == 1 { "article" } else { "articles" };
    format!("({n} {unit} / {hm})")
}

/// `_ngettext("(%d feed)", "(%d feeds)", n)`.
fn nfeeds(n: i64) -> String {
    let unit = if n == 1 { "feed" } else { "feeds" };
    format!("({n} {unit})")
}

/// All of the user's feed categories, ordered `(order_id, title)`.
async fn fetch_all_cats(pool: &sqlx::PgPool, uid: i64) -> Vec<CatRow> {
    let rows = sqlx::query(
        "SELECT id, title, parent_cat FROM ttrss_feed_categories
         WHERE owner_uid = $1 ORDER BY order_id, title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| CatRow {
            id: r.get::<i32, _>(0) as i64,
            title: r.get::<String, _>(1),
            parent_cat: r
                .try_get::<Option<i32>, _>(2)
                .ok()
                .flatten()
                .map(|p| p as i64),
        })
        .collect()
}

/// All of the user's feeds (+ article count), ordered `(order_id, title)`.
async fn fetch_all_feeds(pool: &sqlx::PgPool, uid: i64) -> Vec<FeedRow> {
    let rows = sqlx::query(
        "SELECT f.id, f.title, COALESCE(f.last_error, ''), f.update_interval,
                SUBSTRING_FOR_DATE(f.last_updated, 1, 19) AS last_updated,
                COUNT(ue.int_id) AS num_articles, f.cat_id
         FROM ttrss_feeds f
         LEFT JOIN ttrss_user_entries ue ON ue.feed_id = f.id
         WHERE f.owner_uid = $1
         GROUP BY f.id, f.cat_id
         ORDER BY f.order_id, f.title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| FeedRow {
            id: r.get::<i32, _>(0) as i64,
            title: r.get::<String, _>(1),
            last_error: r.get::<String, _>(2),
            update_interval: r.get::<i32, _>(3) as i64,
            last_updated: r.get::<Option<String>, _>(4),
            num_articles: r.get::<i64, _>(5),
            cat_id: r
                .try_get::<Option<i32>, _>(6)
                .ok()
                .flatten()
                .map(|c| c as i64),
        })
        .collect()
}

/// `Feeds::_get_counters(feed, is_cat=false, unread_only=true, uid)`.
async fn counters(pool: &sqlx::PgPool, uid: i64, feed_id: i64, fresh_max_age: i64) -> i64 {
    if feed_id == FEED_RECENTLY_READ {
        return 0;
    }
    // Labels (`feed < LABEL_BASE_INDEX`).
    if feed_id < LABEL_BASE_INDEX {
        let label_id = crate::labels::feed_to_label_id(feed_id);
        return sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM ttrss_user_entries ue
             JOIN ttrss_user_labels2 ul2 ON ul2.article_id = ue.ref_id
             WHERE ue.unread = true AND ue.owner_uid = $1 AND ul2.label_id = $2",
        )
        .bind(uid)
        .bind(label_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    }

    let (match_part, need_entries) = match feed_id {
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
            if n == FEED_ARCHIVED {
                ("feed_id IS NULL".to_string(), false)
            } else {
                (format!("feed_id = {n}"), false)
            }
        }
        _ => ("true".to_string(), false),
    };

    let sql = if need_entries {
        format!(
            "SELECT count(int_id) FROM ttrss_user_entries, ttrss_entries
             WHERE unread = true
               AND ttrss_entries.id = ttrss_user_entries.ref_id
               AND ({match_part})
               AND ttrss_user_entries.owner_uid = $1"
        )
    } else {
        format!(
            "SELECT count(int_id) FROM ttrss_user_entries
             WHERE unread = true AND ({match_part}) AND ttrss_user_entries.owner_uid = $1"
        )
    };
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Feed configuration mutations (M4: feed CRUD)
//
// Ports the `Pref_Feeds` write surface: `editSave` / `batchEditSave` (both
// delegating to `editsaveops`), `remove` (per-id `remove_feed`), and the
// category add/rename/remove trio. Plugin hooks are no-ops in this port.
// ---------------------------------------------------------------------------

/// `Pref_Feeds::editsaveops($batch)` — the shared single-feed and batch feed
/// save. Single: 0 or 1; batch: total rows touched.
async fn editsaveops(
    state: &AppState,
    sess: &Sess,
    uid: i64,
    profile: Option<i32>,
    extra: &Map<String, Value>,
    batch: bool,
) -> i64 {
    let pool = state.db.pool();
    let s = |k: &str| {
        extra
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let feed_title = util::clean_str(&s("title"));
    let feed_url = util::clean_str(&s("feed_url"));
    let site_url = util::clean_str(&s("site_url"));
    let upd_intl = util::php_int_cast(&s("update_interval"));
    let purge_intl = util::php_int_cast(&s("purge_interval"));
    let feed_id = util::clean_int(extra.get("id")) as i32;
    let feed_ids = if batch {
        util::param_int_array(extra, "ids")
    } else {
        Vec::new()
    };
    let cat_id = util::clean_int(extra.get("cat_id")) as i32;
    let auth_login = util::clean_str(&s("auth_login"));
    let mut auth_pass = util::clean_str(&s("auth_pass"));
    let private = util::checkbox(extra, "private");
    let include_in_digest = util::checkbox(extra, "include_in_digest");
    let cache_images = util::checkbox(extra, "cache_images");
    let hide_images = util::checkbox(extra, "hide_images");
    let always_display_enclosures = util::checkbox(extra, "always_display_enclosures");
    let mark_unread_on_update = util::checkbox(extra, "mark_unread_on_update");
    let feed_language = util::clean_str(&s("feed_language"));

    // Encrypt the password up front (PHP: before the single/batch branch).
    if let Some(key) = &state.config.encryption_key {
        if !auth_pass.is_empty() {
            if let Ok(env) = crate::core::session::encrypt_envelope(auth_pass.as_bytes(), key) {
                auth_pass = base64::engine::general_purpose::STANDARD.encode(env);
            }
        }
    }

    let touched: i64;
    if !batch {
        let exists = sqlx::query_scalar::<_, i32>(
            "SELECT id FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2",
        )
        .bind(feed_id)
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some();
        if !exists {
            return 0;
        }
        let cat: Option<i32> = (cat_id != 0).then_some(cat_id);
        let _ = sqlx::query(
            "UPDATE ttrss_feeds SET
                 title = $1, cat_id = $2, feed_url = $3, site_url = $4,
                 update_interval = $5, purge_interval = $6, auth_login = $7, auth_pass = $8,
                 private = $9, cache_images = $10, hide_images = $11, feed_language = $12,
                 include_in_digest = $13, always_display_enclosures = $14, mark_unread_on_update = $15
             WHERE id = $16 AND owner_uid = $17",
        )
        .bind(&feed_title)
        .bind(cat)
        .bind(&feed_url)
        .bind(&site_url)
        .bind(upd_intl as i32)
        .bind(purge_intl as i32)
        .bind(&auth_login)
        .bind(&auth_pass)
        .bind(private)
        .bind(cache_images)
        .bind(hide_images)
        .bind(&feed_language)
        .bind(include_in_digest)
        .bind(always_display_enclosures)
        .bind(mark_unread_on_update)
        .bind(feed_id)
        .bind(uid)
        .execute(pool)
        .await;
        touched = 1;
        // M7.15 — the HOOK_PREFS_SAVE_FEED chain (the per-feed plugin save,
        // e.g. the af_psql_trgm enable/disable). PHP: run_hooks(HOOK_PREFS_SAVE_FEED,
        // $feed_id) after the feed is saved. M7.22 item 9: dispatch over the
        // *active* plugin set (`session_ctx`) — this chain was once built with
        // `&[], &[]`, iterating zero plugins.
        {
            let mut host_ctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
            host_ctx.run_hooks(HOOK_PREFS_SAVE_FEED, |plugin, host| {
                Box::pin(async move { plugin.hook_prefs_save_feed(host, feed_id, &Value::Object(extra.clone())).await; })
            })
            .await;
        }
        touched
    } else {
        if feed_ids.is_empty() {
            return 0;
        }
        let rows = prefs::load_rows(&state.db, uid, profile).await;
        let enable_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &rows, profile).as_bool();
        let mut tx = pool.begin().await.expect("begin");
        let mut touched: i64 = 0;
        if extra.contains_key("update_interval") {
            let sql = batch_update_sql("update_interval = $1", &feed_ids);
            touched += run_val(&mut tx, &sql, upd_intl as i32, &feed_ids, uid).await;
        }
        if extra.contains_key("purge_interval") {
            let sql = batch_update_sql("purge_interval = $1", &feed_ids);
            touched += run_val(&mut tx, &sql, purge_intl as i32, &feed_ids, uid).await;
        }
        if extra.contains_key("auth_login") {
            let sql = batch_update_sql("auth_login = $1", &feed_ids);
            touched += run_str(&mut tx, &sql, &auth_login, &feed_ids, uid).await;
        }
        if extra.contains_key("auth_pass") {
            let sql = batch_update_sql("auth_pass = $1, auth_pass_encrypted = false", &feed_ids);
            touched += run_str(&mut tx, &sql, &auth_pass, &feed_ids, uid).await;
        }
        if extra.contains_key("private") {
            let sql = batch_update_sql("private = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, private, &feed_ids, uid).await;
        }
        if extra.contains_key("include_in_digest") {
            let sql = batch_update_sql("include_in_digest = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, include_in_digest, &feed_ids, uid).await;
        }
        if extra.contains_key("always_display_enclosures") {
            let sql = batch_update_sql("always_display_enclosures = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, always_display_enclosures, &feed_ids, uid).await;
        }
        if extra.contains_key("mark_unread_on_update") {
            let sql = batch_update_sql("mark_unread_on_update = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, mark_unread_on_update, &feed_ids, uid).await;
        }
        if extra.contains_key("cache_images") {
            let sql = batch_update_sql("cache_images = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, cache_images, &feed_ids, uid).await;
        }
        if extra.contains_key("hide_images") {
            let sql = batch_update_sql("hide_images = $1", &feed_ids);
            touched += run_bool(&mut tx, &sql, hide_images, &feed_ids, uid).await;
        }
        if enable_cats && extra.contains_key("cat_id") {
            let sql = batch_update_sql("cat_id = $1", &feed_ids);
            touched += run_opt(
                &mut tx,
                &sql,
                (cat_id != 0).then_some(cat_id),
                &feed_ids,
                uid,
            )
            .await;
        }
        if extra.contains_key("feed_language") {
            let sql = batch_update_sql("feed_language = $1", &feed_ids);
            touched += run_str(&mut tx, &sql, &feed_language, &feed_ids, uid).await;
        }
        tx.commit().await.expect("commit");
        touched
    }
}

/// `UPDATE ttrss_feeds SET <set> WHERE id IN ($2..$n+1) AND owner_uid = $(n+2)`.
/// `<set>` uses `$1` for the value.
fn batch_update_sql(set_expr: &str, feed_ids: &[i32]) -> String {
    let ids = (2..=feed_ids.len() + 1)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let uid_pos = feed_ids.len() + 2;
    format!("UPDATE ttrss_feeds SET {set_expr} WHERE id IN ({ids}) AND owner_uid = ${uid_pos}")
}

async fn run_val(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    v: i32,
    feed_ids: &[i32],
    uid: i64,
) -> i64 {
    let mut q = sqlx::query(AssertSqlSafe(sql)).bind(v);
    for id in feed_ids {
        q = q.bind(*id);
    }
    q.bind(uid)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() as i64)
        .unwrap_or(0)
}

async fn run_str(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    v: &str,
    feed_ids: &[i32],
    uid: i64,
) -> i64 {
    let mut q = sqlx::query(AssertSqlSafe(sql)).bind(v);
    for id in feed_ids {
        q = q.bind(*id);
    }
    q.bind(uid)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() as i64)
        .unwrap_or(0)
}

async fn run_bool(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    v: bool,
    feed_ids: &[i32],
    uid: i64,
) -> i64 {
    let mut q = sqlx::query(AssertSqlSafe(sql)).bind(v);
    for id in feed_ids {
        q = q.bind(*id);
    }
    q.bind(uid)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() as i64)
        .unwrap_or(0)
}

async fn run_opt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    v: Option<i32>,
    feed_ids: &[i32],
    uid: i64,
) -> i64 {
    let mut q = sqlx::query(AssertSqlSafe(sql)).bind(v);
    for id in feed_ids {
        q = q.bind(*id);
    }
    q.bind(uid)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() as i64)
        .unwrap_or(0)
}

/// `Pref_Feeds::editSave` — save one feed's config (`editsaveops(false)`).
pub async fn edit_save(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let _ = editsaveops(
        state,
        sess,
        data.uid().unwrap_or(0),
        data.profile().map(|p| p as i32),
        extra,
        false,
    )
    .await;
    Value::Null
}

/// `Pref_Feeds::batchEditSave` — batch-save feed config (`editsaveops(true)`).
pub async fn batch_edit_save(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let _ = editsaveops(
        state,
        sess,
        data.uid().unwrap_or(0),
        data.profile().map(|p| p as i32),
        extra,
        true,
    )
    .await;
    Value::Null
}

/// `Pref_Feeds::remove` — remove each feed/label id (no-op response).
pub async fn remove_feeds(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    for id in util::param_int_array(extra, "ids") {
        remove_feed(state, id, uid).await;
    }
    Value::Null
}

/// `Feeds::remove_feed` — delete a feed (positive id) or a label (negative id).
pub async fn remove_feed(state: &AppState, id: i32, uid: i64) {
    let pool = state.db.pool();
    if id > 0 {
        let mut tx = pool.begin().await.expect("begin");
        // Starred (archived) entries → archived (feed_id = NULL).
        sqlx::query(
            "UPDATE ttrss_user_entries SET feed_id = NULL, orig_feed_id = NULL
                     WHERE feed_id = $1 AND marked = true AND owner_uid = $2",
        )
        .bind(id)
        .bind(uid)
        .execute(&mut *tx)
        .await
        .ok();
        // Access keys for the feed.
        sqlx::query("DELETE FROM ttrss_access_keys WHERE feed_id = $1 AND owner_uid = $2")
            .bind(id.to_string())
            .bind(uid)
            .execute(&mut *tx)
            .await
            .ok();
        // The feed itself.
        sqlx::query("DELETE FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2")
            .bind(id)
            .bind(uid)
            .execute(&mut *tx)
            .await
            .ok();
        tx.commit().await.expect("commit");
        // Best-effort favicon-cache removal (PHP: `DiskCache('feed-icons')->remove`).
        let _ = std::fs::remove_file(state.config.feed_icon_dir.join(id.to_string()));
    } else {
        let label_id = crate::labels::feed_to_label_id(id as i64);
        let _ = crate::labels::remove(&state.db, label_id as i32, uid, Some(uid)).await;
    }
}

/// `Pref_Feeds::addCat` — add a feed category (`Feeds::_add_cat`).
pub async fn add_cat(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let title = util::clean_str(extra.get("cat").and_then(|v| v.as_str()).unwrap_or(""));
    let pool = state.db.pool();
    // Find an existing category for this owner + parent (NULL) + title.
    let existing = sqlx::query_scalar::<_, i32>(
        "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND parent_cat IS NULL AND title = $2",
    )
    .bind(uid)
    .bind(&title)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if existing.is_none() {
        sqlx::query("INSERT INTO ttrss_feed_categories (owner_uid, parent_cat, order_id, title) VALUES ($1, NULL, $2, $3)")
            .bind(uid)
            .bind(0)
            .bind(&title)
            .execute(pool)
            .await
            .ok();
    }
    Value::Null
}

/// `Pref_Feeds::renameCat` — rename a category if it exists and the title is set.
pub async fn rename_cat(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let id = util::clean_int(extra.get("id")) as i32;
    let title = util::clean_str(extra.get("title").and_then(|v| v.as_str()).unwrap_or(""));
    let pool = state.db.pool();
    if !title.is_empty() {
        let found = sqlx::query_scalar::<_, i32>(
            "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND id = $2",
        )
        .bind(uid)
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some();
        if found {
            sqlx::query(
                "UPDATE ttrss_feed_categories SET title = $1 WHERE owner_uid = $2 AND id = $3",
            )
            .bind(&title)
            .bind(uid)
            .bind(id)
            .execute(pool)
            .await
            .ok();
        }
    }
    Value::Null
}

pub async fn remove_cat(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    for id in util::param_int_array(extra, "ids") {
        // `Feeds::_remove_cat`: find the category (owner-scoped) and delete it
        // (feeds keep their rows; their `cat_id` is nulled via `ON DELETE SET NULL`).
        let found = sqlx::query_scalar::<_, i32>(
            "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND id = $2",
        )
        .bind(uid)
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some();
        if found {
            sqlx::query("DELETE FROM ttrss_feed_categories WHERE owner_uid = $1 AND id = $2")
                .bind(uid)
                .bind(id)
                .execute(pool)
                .await
                .ok();
        }
    }
    Value::Null
}

// ============================================================================
// Access keys (M4: feed CRUD)
//
// `Pref_Feeds::clearKeys` / `regenFeedKey` / `getSharedURL` and their
// `Feeds::_{clear,update,get}_access_key` helpers. These manage the per-feed
// (or per-category) random keys that let a reader fetch a feed's articles via
// the public `public.php?op=rss&id=..&key=..` endpoint (M4).
// ============================================================================

/// `Feeds::_get_access_key` — the access key for (owner, feed_id, is_cat),
/// creating one with a fresh `uniqid_short()` if absent.
async fn get_access_key(
    pool: &sqlx::PgPool,
    feed_id: &str,
    is_cat: bool,
    owner_uid: i64,
) -> Option<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT access_key FROM ttrss_access_keys \
         WHERE owner_uid = $1 AND feed_id = $2 AND is_cat = $3",
    )
    .bind(owner_uid)
    .bind(feed_id)
    .bind(is_cat)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if let Some(key) = existing {
        return Some(key);
    }
    let key = crate::util::uniqid_short();
    sqlx::query(
        "INSERT INTO ttrss_access_keys (access_key, feed_id, is_cat, owner_uid) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&key)
    .bind(feed_id)
    .bind(is_cat)
    .bind(owner_uid)
    .execute(pool)
    .await
    .ok()
    .map(|_| key)
}

/// `Feeds::_update_access_key` — drop the existing key and return a fresh one.
async fn update_access_key(
    pool: &sqlx::PgPool,
    feed_id: &str,
    is_cat: bool,
    owner_uid: i64,
) -> Option<String> {
    let _ = sqlx::query(
        "DELETE FROM ttrss_access_keys \
         WHERE owner_uid = $1 AND feed_id = $2 AND is_cat = $3",
    )
    .bind(owner_uid)
    .bind(feed_id)
    .bind(is_cat)
    .execute(pool)
    .await;
    get_access_key(pool, feed_id, is_cat, owner_uid).await
}

/// `Feeds::_clear_access_keys` — delete every access key for the owner.
async fn clear_access_keys(pool: &sqlx::PgPool, owner_uid: i64) {
    let _ = sqlx::query("DELETE FROM ttrss_access_keys WHERE owner_uid = $1")
        .bind(owner_uid)
        .execute(pool)
        .await;
}

/// PHP `http_build_query` / `urlencode` (query-string form): preserves
/// alphanumerics and `-_.`, encodes a space as `+`, and `%XX`s the rest.
fn php_urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// PHP `http_build_query` for a flat `&`-separated list of key/value pairs
/// (empty values kept, e.g. `q=`).
fn http_build_query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", php_urlencode(k), php_urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `Pref_Feeds::clearKeys` — clear every access key for the user.
pub async fn clear_keys(state: &AppState, sess: &Sess, _extra: &Map<String, Value>) -> Value {
    clear_access_keys(state.db.pool(), sess.data().uid().unwrap_or(0)).await;
    Value::Null
}

/// `Pref_Feeds::regenFeedKey` — regenerate the (feed|category) access key.
pub async fn regen_feed_key(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = util::clean_str(
        extra
            .get("id")
            .and_then(util::scalar_str)
            .as_deref()
            .unwrap_or(""),
    );
    let is_cat = util::param_to_bool(extra.get("is_cat"));
    let key = update_access_key(state.db.pool(), &feed_id, is_cat, uid).await;
    json!({ "link": key })
}

/// `Pref_Feeds::getSharedURL` — the public RSS share URL for a feed/category.
pub async fn get_shared_url(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = util::clean_str(
        extra
            .get("id")
            .and_then(util::scalar_str)
            .as_deref()
            .unwrap_or(""),
    );
    let is_cat = util::param_to_bool(extra.get("is_cat"));
    let search = util::clean_str(
        extra
            .get("search")
            .and_then(util::scalar_str)
            .as_deref()
            .unwrap_or(""),
    );
    let key = get_access_key(state.db.pool(), &feed_id, is_cat, uid).await;
    let is_cat_s = if is_cat { "1" } else { "0" };
    let id = feed_id.parse::<i64>().unwrap_or(0);
    let title = if is_cat {
        category_title(state.db.pool(), id, uid).await
    } else {
        feed_title(state.db.pool(), id, uid).await
    };
    let link = format!(
        "{}/public.php?{}",
        state.config.self_url_rtrim(),
        http_build_query(&[
            ("op", "rss"),
            ("id", &feed_id),
            ("is_cat", is_cat_s),
            ("q", &search),
            ("key", key.as_deref().unwrap_or("")),
        ])
    );
    json!({ "title": title, "link": link })
}

// ============================================================================
// Ordering (M4: feed CRUD)
//
// `Pref_Feeds::catsortreset` / `feedsortreset` / `savefeedorder` — reset and
// persist the feed/category drag-and-drop order from the dojo feed tree.
// ============================================================================

/// PHP `substr($id, strpos($id, ":") + 1)` — the bare numeric id after the
/// `PREFIX:` marker (for ids without `:`, `strpos` is `false` → `substr($id, 1)`;
/// empty string → empty string, matching PHP).
fn bare_id(s: &str) -> &str {
    match s.find(':') {
        Some(pos) => &s[pos + 1..],
        None => s.get(1..).unwrap_or(""),
    }
}

/// `Pref_Feeds::catsortreset` — reset every category order position to 0.
pub async fn catsortreset(state: &AppState, sess: &Sess, _extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    sqlx::query("UPDATE ttrss_feed_categories SET order_id = 0 WHERE owner_uid = $1")
        .bind(uid)
        .execute(state.db.pool())
        .await
        .ok();
    Value::Null
}

/// `Pref_Feeds::feedsortreset` — reset every feed order position to 0.
pub async fn feedsortreset(state: &AppState, sess: &Sess, _extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    sqlx::query("UPDATE ttrss_feeds SET order_id = 0 WHERE owner_uid = $1")
        .bind(uid)
        .execute(state.db.pool())
        .await
        .ok();
    Value::Null
}

/// `Pref_Feeds::process_category_order` — walk the dojo tree and persist each
/// feed's / category's `order_id` (and `cat_id` / `parent_cat`) for the owner.
async fn process_category_order(
    state: &AppState,
    uid: i64,
    data_map: &Map<String, Value>,
    root_item: &str,
) {
    let pool = state.db.pool();
    // Iterative (stack-based) walk, equivalent to the PHP recursion: a category
    // node sets its own `parent_cat` when its frame is popped, while its parent
    // assigns its `order_id` (the position in the parent's list).
    let mut stack: Vec<(String, String)> = vec![(root_item.to_string(), String::new())];
    while let Some((item_id, parent_id)) = stack.pop() {
        let bare_item_id = bare_id(&item_id);
        if item_id != "root" {
            let parent_qpart: Option<i32> = if !parent_id.is_empty() && parent_id != "root" {
                Some(bare_id(&parent_id).parse().unwrap_or(0))
            } else {
                None
            };
            let bid = bare_item_id.parse::<i32>().unwrap_or(0);
            let cat_exists = sqlx::query_scalar::<_, i32>(
                "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND id = $2",
            )
            .bind(uid)
            .bind(bid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .is_some();
            if cat_exists {
                sqlx::query(
                    "UPDATE ttrss_feed_categories SET parent_cat = $1 \
                     WHERE owner_uid = $2 AND id = $3",
                )
                .bind(parent_qpart)
                .bind(uid)
                .bind(bid)
                .execute(pool)
                .await
                .ok();
            }
        }
        let mut order_id: i32 = 1;
        if let Some(Value::Array(children)) = data_map.get(&item_id) {
            for item in children {
                let id = item
                    .get("_reference")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let bare = bare_id(id);
                if !id.is_empty() {
                    if id.starts_with("FEED") {
                        let bid = bare.parse::<i32>().unwrap_or(0);
                        let feed_exists = sqlx::query_scalar::<_, i32>(
                            "SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND id = $2",
                        )
                        .bind(uid)
                        .bind(bid)
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                        if feed_exists {
                            let cat_id: Option<i32> =
                                if item_id != "root" && !bare_item_id.is_empty() {
                                    Some(bare_item_id.parse().unwrap_or(0))
                                } else {
                                    None
                                };
                            sqlx::query(
                                "UPDATE ttrss_feeds SET order_id = $1, cat_id = $2 \
                                 WHERE owner_uid = $3 AND id = $4",
                            )
                            .bind(order_id)
                            .bind(cat_id)
                            .bind(uid)
                            .bind(bid)
                            .execute(pool)
                            .await
                            .ok();
                        }
                    } else if id.starts_with("CAT:") {
                        let bid = bare.parse::<i32>().unwrap_or(0);
                        let cat_exists = sqlx::query_scalar::<_, i32>(
                            "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND id = $2",
                        )
                        .bind(uid)
                        .bind(bid)
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                        if cat_exists {
                            sqlx::query(
                                "UPDATE ttrss_feed_categories SET order_id = $1 \
                                 WHERE owner_uid = $2 AND id = $3",
                            )
                            .bind(order_id)
                            .bind(uid)
                            .bind(bid)
                            .execute(pool)
                            .await
                            .ok();
                        }
                        stack.push((id.to_string(), item_id.clone()));
                    }
                }
                order_id += 1;
            }
        }
    }
}

/// `Pref_Feeds::savefeedorder` — persist the dojo feed-tree order (`payload`).
pub async fn savefeedorder(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let payload = extra.get("payload").and_then(|v| v.as_str()).unwrap_or("");
    let data: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    // `$data['items']` — already an array, or a JSON-encoded string to decode.
    let items: Value = match &data {
        Value::Object(m) => {
            let raw = m.get("items").cloned().unwrap_or(Value::Null);
            if raw.is_array() {
                raw
            } else {
                serde_json::from_str(raw.as_str().unwrap_or("")).unwrap_or(Value::Null)
            }
        }
        _ => Value::Null,
    };
    if let Some(items_list) = items.as_array() {
        let mut data_map: Map<String, Value> = Map::new();
        let mut root_item = String::new();
        for item in items_list {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(items_field) = item.get("items").filter(|v| v.is_array() || v.is_object()) {
                let val = if items_field.get("_reference").is_some() {
                    json!([items_field])
                } else {
                    items_field.clone()
                };
                data_map.insert(id.to_string(), val);
            }
            if id == "root" {
                root_item = "root".to_string();
            }
        }
        process_category_order(state, uid, &data_map, &root_item).await;
    }
    Value::Null
}

// ============================================================================
// Icons (M4: feed CRUD)
//
// `Pref_Feeds::removeIcon` — drop the feed's custom favicon cache and reset the
// `Pref_Feeds::removeIcon` / `uploadIcon` — manage the feed's custom favicon
// ============================================================================

/// `Pref_Feeds::removeIcon` — delete the feed's favicon cache and reset its
/// `favicon_*` columns (only if the cached file exists and is removable).
pub async fn remove_icon(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = util::clean_int(extra.get("feed_id"));
    let pool = state.db.pool();
    let feed: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND id = $2")
            .bind(uid)
            .bind(feed_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if let Some(fid) = feed {
        let cache_file = state.config.feed_icon_dir.join(fid.to_string());
        if cache_file.exists() && fs::remove_file(&cache_file).is_ok() {
            sqlx::query(
                "UPDATE ttrss_feeds SET favicon_avg_color = NULL, \
                 favicon_last_checked = '1970-01-01', favicon_is_custom = false \
                 WHERE owner_uid = $1 AND id = $2",
            )
            .bind(uid)
            .bind(fid)
            .execute(pool)
            .await
            .ok();
        }
    }
    Value::Null
}

/// `Pref_Feeds::uploadIcon` — persist an uploaded custom favicon to the
/// `feed-icons` cache and flag the feed. Returns `{rc, icon_url}` where `rc`
/// is one of the `E_ICON_*` markers. The PHP temp-file + `move_uploaded_file`
/// dance is elided — the multipart bytes are already in hand.
pub async fn upload_icon(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
    file: Option<&[u8]>,
) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = util::clean_int(extra.get("feed_id"));
    let pool = state.db.pool();

    // Default (PHP `self::E_ICON_UPLOAD_FAILED`).
    let mut rc = "E_ICON_UPLOAD_FAILED".to_string();

    let feed: Option<i32> =
        sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND id = $2")
            .bind(uid)
            .bind(feed_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    if let (Some(fid), Some(bytes)) = (feed, file) {
        if bytes.len() < state.config.max_favicon_file_size {
            let dir = &state.config.feed_icon_dir;
            let cache_file = dir.join(fid.to_string());
            let written = fs::create_dir_all(dir)
                .ok()
                .and_then(|_| fs::write(&cache_file, bytes).ok())
                .is_some();
            if written {
                let n = sqlx::query(
                    "UPDATE ttrss_feeds SET favicon_avg_color = NULL, \
                     favicon_is_custom = true WHERE owner_uid = $1 AND id = $2",
                )
                .bind(uid)
                .bind(fid)
                .execute(pool)
                .await
                .ok();
                if n.map(|r| r.rows_affected() > 0).unwrap_or(false) {
                    rc = "E_ICON_UPLOAD_SUCCESS".to_string();
                }
            } else {
                rc = "E_ICON_RENAME_FAILED".to_string();
            }
        } else {
            rc = "E_ICON_FILE_TOO_LARGE".to_string();
        }
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    json!({
        "rc": rc,
        "icon_url": format!("{}?ts={}", feed_icon_url(state, feed_id), ts),
    })
}

/// `Feeds::decrypt_feed_pass` — reverse the stored `auth_pass` envelope (base64 →
/// PHP-unserialized `{algo,nonce,payload}` → decrypt) to the plaintext password.
/// Returns the input unchanged when it can't be decoded.
pub fn decrypt_feed_pass(auth_pass: &str, key: Option<&[u8; 32]>) -> String {
    if !auth_pass.is_empty() {
        if let Some(k) = key {
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(auth_pass) {
                if let Ok(pt) = crate::core::session::decrypt_envelope(&decoded, k) {
                    return String::from_utf8_lossy(&pt).to_string();
                }
            }
        }
    }
    auth_pass.to_string()
}

/// `Feeds::_get_icon` — the Material icon name for a virtual feed id, or the
/// feed's icon URL for a real feed id ("" when it has no icon).
pub fn get_icon(state: &AppState, feed_id: i64) -> String {
    match feed_id {
        0 => "archive".to_string(),
        -1 => "star".to_string(),
        -2 => "rss_feed".to_string(),
        -3 => "whatshot".to_string(),
        -4 => "inbox".to_string(),
        -6 => "restore".to_string(),
        _ if feed_id < crate::labels::LABEL_BASE_INDEX => "label".to_string(),
        _ => feed_icon_url(state, feed_id),
    }
}

/// `Controls::select_feeds_cats` — a `<select>` of the user's feed categories,
/// nested by `parent_cat` (indented by level), with a disabled separator and an
/// "Uncategorized" option (value 0) at the end. `default_id` is pre-selected;
/// `attributes` is a raw trailing attribute string (e.g. `disabled="1"`).
pub async fn select_feeds_cats(
    pool: &sqlx::PgPool,
    uid: i64,
    name: &str,
    default_id: Option<i32>,
    attributes: &str,
) -> String {
    let rows: Vec<(i32, String, Option<i32>)> = sqlx::query_as(
        "SELECT id, title, parent_cat FROM ttrss_feed_categories \nWHERE owner_uid = $1 ORDER BY title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    // Group by parent (None = top-level). A single query ordered by title keeps
    // each group in title order (matching the PHP's per-level `ORDER BY title`).
    let mut by_parent: HashMap<Option<i32>, Vec<(i32, String)>> = HashMap::new();
    for (id, title, parent) in &rows {
        by_parent
            .entry(*parent)
            .or_default()
            .push((*id, title.clone()));
    }
    let mut ret = String::new();
    ret.push_str(&format!(
        "<select name=\"{}\"\n                        id=\"\"\n                        default=\"{}\"\n                        dojoType=\"fox.form.Select\" {}>",
        util::htmlspecialchars(name),
        default_id.map(|d| d.to_string()).unwrap_or_default(),
        attributes
    ));
    let mut found = 0usize;
    // Pre-order DFS with an explicit stack; children are pushed in reverse so the
    // first child is emitted first.
    let mut stack: Vec<(i32, i32)> = by_parent
        .get(&None)
        .map(|cats| cats.iter().rev().map(|(id, _)| (*id, 0)).collect())
        .unwrap_or_default();
    while let Some((id, level)) = stack.pop() {
        let title = rows
            .iter()
            .find(|(rid, _, _)| *rid == id)
            .map(|(_, t, _)| t);
        if let Some(title) = title {
            found += 1;
            let is_selected = if Some(id) == default_id {
                "selected=\"1\""
            } else {
                ""
            };
            let mut indented = title.to_string();
            for _ in 0..level {
                indented.insert(0, '\u{2003}');
            }
            if !title.is_empty() {
                ret.push_str(&format!(
                    "<option {is_selected} value='{id}'>{}</option>",
                    util::htmlspecialchars(&indented)
                ));
            }
            if let Some(children) = by_parent.get(&Some(id)) {
                for (cid, _) in children.iter().rev() {
                    stack.push((*cid, level + 1));
                }
            }
        }
    }
    if found > 0 {
        ret.push_str(&format!(
            "<option disabled=\"1\">{}</option>",
            "\u{2015}".repeat(15)
        ));
    }
    // PHP `$default_id == 0`: `null == 0` is true, so a null default (the
    // `editfeeds` batch form) also selects `Uncategorized`.
    let is_selected = if default_id.is_none() || default_id == Some(0) {
        "selected=\"1\""
    } else {
        ""
    };
    ret.push_str(&format!(
        "<option {is_selected} value=\"0\">Uncategorized</option>"
    ));
    ret.push_str("</select>");
    ret
}

/// `Pref_Feeds::get_ts_languages` — the installed Postgres text-search
/// configurations, each with its first letter uppercased.
pub async fn get_ts_languages(pool: &sqlx::PgPool) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT cfgname FROM pg_ts_config")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    rows.iter().map(|s| ucfirst(s)).collect()
}

/// `ucfirst` — uppercase the first character of `s`.
fn ucfirst(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// The full `ttrss_feeds` row (all 40 columns) as returned by the PHP `editfeed`
/// form data. The six timestamp columns are cast to `::text` to match the PDO
/// string representation the PHP `as_array()` produces.
#[derive(sqlx::FromRow, Serialize)]
struct FeedFullRow {
    id: i32,
    owner_uid: i32,
    title: String,
    cat_id: Option<i32>,
    feed_url: String,
    icon_url: String,
    update_interval: i32,
    purge_interval: i32,
    last_updated: Option<String>,
    last_unconditional: Option<String>,
    last_error: String,
    last_modified: String,
    favicon_avg_color: Option<String>,
    favicon_is_custom: Option<bool>,
    site_url: String,
    auth_login: String,
    parent_feed: Option<i32>,
    private: bool,
    auth_pass: String,
    hidden: bool,
    include_in_digest: bool,
    rtl_content: bool,
    cache_images: bool,
    hide_images: bool,
    cache_content: bool,
    last_viewed: Option<String>,
    last_update_started: Option<String>,
    last_successful_update: Option<String>,
    update_method: i32,
    always_display_enclosures: bool,
    order_id: i32,
    mark_unread_on_update: bool,
    update_on_checksum_change: bool,
    strip_images: bool,
    view_settings: String,
    pubsub_state: i32,
    favicon_last_checked: Option<String>,
    feed_language: String,
    auth_pass_encrypted: bool,
}

/// `Pref_Feeds::editfeed` — the data the single-feed edit form needs: the feed
/// row (with a computed `icon` + decrypted `auth_pass`), the category `<select>`,
/// the update/purge interval options, the user's access level, and the language
/// list. `plugin_data` is empty (no plugins are ported).
pub async fn editfeed(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let feed_id = util::clean_int(extra.get("id"));
    let pool = state.db.pool();
    let row: Option<FeedFullRow> = sqlx::query_as(
        "SELECT id, owner_uid, title, cat_id, feed_url, icon_url, update_interval, purge_interval, last_updated::text AS last_updated, last_unconditional::text AS last_unconditional, last_error, last_modified, favicon_avg_color, favicon_is_custom, site_url, auth_login, parent_feed, private, auth_pass, hidden, include_in_digest, rtl_content, cache_images, hide_images, cache_content, last_viewed::text AS last_viewed, last_update_started::text AS last_update_started, last_successful_update::text AS last_successful_update, update_method, always_display_enclosures, order_id, mark_unread_on_update, update_on_checksum_change, strip_images, view_settings, pubsub_state, favicon_last_checked::text AS favicon_last_checked, feed_language, auth_pass_encrypted FROM ttrss_feeds WHERE owner_uid = $1 AND id = $2",
    )
    .bind(uid)
    .bind(feed_id as i32)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return Value::Null;
    };
    let pref_rows = prefs::load_rows(&state.db, uid, None).await;

    // The PHP `as_array()` returns the whole row; serialize it in column order,
    // then override `auth_pass` (decrypted) and add the computed `icon`.
    let mut feed: Value = serde_json::to_value(&row).expect("feed row");
    {
        let obj = feed.as_object_mut().unwrap();
        obj.insert(
            "auth_pass".into(),
            json!(decrypt_feed_pass(
                &row.auth_pass,
                state.config.encryption_key.as_ref()
            )),
        );
        obj.insert("icon".into(), json!(get_icon(state, feed_id)));
    }

    // update intervals (key => label), with the "Default interval" option annotated.
    let default_update = prefs::resolve("DEFAULT_UPDATE_INTERVAL", &pref_rows, None).as_int();
    let update = update_intervals_with_default(default_update);

    // purge intervals.
    let purge: Value = if state.config.force_article_purge == 0 {
        let mut m: Map<String, Value> = Map::new();
        for (k, v) in [
            ("0", "Use default"),
            ("-1", "Never purge"),
            ("7", "1 week old"),
            ("14", "2 weeks old"),
            ("31", "1 month old"),
            ("60", "2 months old"),
            ("90", "3 months old"),
        ] {
            m.insert(k.to_string(), Value::String(v.to_string()));
        }
        let default_purge = prefs::resolve(prefs::PURGE_OLD_DAYS, &pref_rows, None).as_int();
        let suffix = if default_purge > 0 {
            format!(
                "({default_purge} day{})",
                if default_purge == 1 { "" } else { "s" }
            )
        } else {
            "(Disabled)".to_string()
        };
        m.insert(
            "0".to_string(),
            Value::String(format!("Use default {suffix}")),
        );
        Value::Object(m)
    } else {
        let fp = state.config.force_article_purge;
        Value::Array(vec![Value::String(format!(
            "{fp} day{}",
            if fp == 1 { "" } else { "s" }
        ))])
    };

    // user access level.
    let access_level: Option<i32> =
        sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    let cats_enabled = prefs::resolve(prefs::ENABLE_FEED_CATS, &pref_rows, profile).as_bool();
    let cats_select = select_feeds_cats(pool, uid, "cat_id", row.cat_id, "").await;
    let default_lang = prefs::resolve(prefs::DEFAULT_SEARCH_LANGUAGE, &pref_rows, profile)
        .as_str()
        .to_string();
    let all_lang = get_ts_languages(pool).await;

    // M7.15 — the HOOK_PREFS_EDIT_FEED chain (the per-feed plugin UI, e.g.
    // the af_psql_trgm checkbox). PHP: ob_start(); run_hooks(...); $plugin_data =
    // trim(ob_get_contents()); ob_end_clean(). M7.22 item 9: dispatch over the
    // *active* plugin set (`session_ctx`) — this chain was once built with
    // `&[], &[]`, iterating zero plugins.
    let mut host_ctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    host_ctx.run_hooks(HOOK_PREFS_EDIT_FEED, |plugin, host| {
        Box::pin(async move { plugin.hook_prefs_edit_feed(host, feed_id as i32).await; })
    })
    .await;
    let plugin_data = host_ctx.output.trim().to_string();

    json!({
        "feed": feed,
        "cats": {
            "enabled": cats_enabled,
            "select": cats_select,
        },
        "plugin_data": plugin_data,
        "force_purge": state.config.force_article_purge,
        "intervals": {
            "update": update,
            "purge": purge,
        },
        "user": {
            "access_level": access_level,
        },
        "lang": {
            "enabled": true,
            "default": default_lang,
            "all": all_lang,
        },
    })
}

/// `Pref_Feeds::editfeeds` — the batch-edit dialog (the read side; the write is
/// `batchEditSave`). Renders the HTML the frontend ingests (a notice, three
/// hidden fields, and a three-tab container). Every editable field carries a
/// disabled input + a "Check to enable field" toggle so only the checked
/// fields are applied on save.
///
/// The browser normalizes whitespace (the content is fed to a `dijit.Dialog`
/// via `innerHTML`), so the HTML lives in `editfeeds.html.jinja` (see
/// AGENTS.md "Frontend HTML forms"); this function just supplies the context.
pub async fn editfeeds(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let pool = state.db.pool();

    let feed_ids = extra
        .get("ids")
        .and_then(Value::as_str)
        .map(util::clean_str)
        .unwrap_or_default();

    let pref_rows = prefs::load_rows(&state.db, uid, None).await;
    let enable_feed_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &pref_rows, profile).as_bool();
    let default_update = prefs::resolve("DEFAULT_UPDATE_INTERVAL", &pref_rows, None).as_int();
    let default_purge = prefs::resolve(prefs::PURGE_OLD_DAYS, &pref_rows, None).as_int();

    // PHP `$update_intervals` / `$purge_intervals` (backend.php), with the
    // default-annotation appended to the `0` key (PHP `editfeeds`).
    let update: Vec<(String, String)> = vec![
        (
            "0".to_string(),
            format!(
                "Default interval ({})",
                update_interval_label(default_update)
            ),
        ),
        ("-1".to_string(), "Disable updates".to_string()),
        ("15".to_string(), "15 minutes".to_string()),
        ("30".to_string(), "30 minutes".to_string()),
        ("60".to_string(), "Hourly".to_string()),
        ("240".to_string(), "4 hours".to_string()),
        ("720".to_string(), "12 hours".to_string()),
        ("1440".to_string(), "Daily".to_string()),
        ("10080".to_string(), "Weekly".to_string()),
    ];
    let purge_default = if default_purge > 0 {
        format!(" ({} days)", default_purge)
    } else {
        " (Disabled)".to_string()
    };
    let purge: Vec<(String, String)> = vec![
        ("0".to_string(), format!("Use default{}", purge_default)),
        ("-1".to_string(), "Never purge".to_string()),
        ("7".to_string(), "1 week old".to_string()),
        ("14".to_string(), "2 weeks old".to_string()),
        ("31".to_string(), "1 month old".to_string()),
        ("60".to_string(), "2 months old".to_string()),
        ("90".to_string(), "3 months old".to_string()),
    ];

    let cat_select = select_feeds_cats(pool, uid, "cat_id", None, "disabled=\"1\"").await;
    let languages = get_ts_languages(pool).await;

    let options: Vec<Value> = [
        ("include_in_digest", "Include in email digest"),
        (
            "always_display_enclosures",
            "Always display image attachments",
        ),
        ("hide_images", "Do not embed media"),
        ("cache_images", "Cache media"),
        ("mark_unread_on_update", "Mark updated articles as unread"),
    ]
    .iter()
    .map(|(name, caption)| {
        json!({
            "checkbox": controls::checkbox_tag(name, false, "", &[("disabled", "1".to_string())]),
            "caption": *caption,
            "toggle": batch_toggle_checkbox(name),
        })
    })
    .collect();

    let mut ctx: Map<String, Value> = Map::new();
    ctx.insert(
        "hidden_ids".into(),
        json!(controls::hidden_tag("ids", &feed_ids)),
    );
    ctx.insert(
        "hidden_op".into(),
        json!(controls::hidden_tag("op", "Pref_Feeds")),
    );
    ctx.insert(
        "hidden_method".into(),
        json!(controls::hidden_tag("method", "batchEditSave")),
    );
    ctx.insert("enable_feed_cats".into(), json!(enable_feed_cats));
    ctx.insert(
        "force_purge".into(),
        json!(state.config.force_article_purge != 0),
    );
    ctx.insert("cat_select".into(), json!(cat_select));
    ctx.insert("toggle_cat".into(), json!(batch_toggle_checkbox("cat_id")));
    ctx.insert(
        "lang_select".into(),
        json!(controls::select_tag(
            "feed_language",
            "",
            &languages,
            &[("disabled", "1".to_string())]
        )),
    );
    ctx.insert(
        "toggle_lang".into(),
        json!(batch_toggle_checkbox("feed_language")),
    );
    ctx.insert(
        "update_select".into(),
        json!(controls::select_hash(
            "update_interval",
            "",
            &update,
            &[("disabled", "1".to_string())]
        )),
    );
    ctx.insert(
        "toggle_update".into(),
        json!(batch_toggle_checkbox("update_interval")),
    );
    ctx.insert(
        "purge_select".into(),
        json!(controls::select_hash(
            "purge_interval",
            "",
            &purge,
            &[("disabled", "1".to_string())]
        )),
    );
    ctx.insert(
        "toggle_purge".into(),
        json!(batch_toggle_checkbox("purge_interval")),
    );
    ctx.insert(
        "toggle_login".into(),
        json!(batch_toggle_checkbox("auth_login")),
    );
    ctx.insert(
        "toggle_pass".into(),
        json!(batch_toggle_checkbox("auth_pass")),
    );
    ctx.insert("options".into(), Value::Array(options));
    ctx.insert("save_btn".into(), json!(controls::submit_tag("Save")));
    ctx.insert(
        "cancel_btn".into(),
        json!(controls::cancel_dialog_tag("Cancel")),
    );

    editfeeds_env()
        .get_template("editfeeds")
        .expect("editfeeds template")
        .render(ctx)
        .expect("editfeeds template render")
}

/// The `editfeeds` "Check to enable field" toggle for a given field name.
fn batch_toggle_checkbox(name: &str) -> String {
    controls::checkbox_tag(
        "",
        false,
        "",
        &[
            ("data-control-for", name.to_string()),
            ("title", "Check to enable field".to_string()),
            (
                "onchange",
                "App.dialogOf(this).toggleField(this)".to_string(),
            ),
        ],
    )
}

/// PHP `update_intervals[$value]` for the `editfeeds` update-interval default
/// annotation; a non-standard value yields an empty string.
fn update_interval_label(value: i64) -> String {
    match value {
        0 => "Default interval".to_string(),
        -1 => "Disable updates".to_string(),
        15 => "15 minutes".to_string(),
        30 => "30 minutes".to_string(),
        60 => "Hourly".to_string(),
        240 => "4 hours".to_string(),
        720 => "12 hours".to_string(),
        1440 => "Daily".to_string(),
        10080 => "Weekly".to_string(),
        _ => String::new(),
    }
}

/// PHP `$update_intervals` (backend.php) as a JSON object (key => label), with
/// the `0` ("Default interval") label annotated with the user's
/// `DEFAULT_UPDATE_INTERVAL` — PHP `subscribeToFeed` / `editfeeds`'
/// `$local_update_intervals[0] .= sprintf(" (%s)", $update_intervals[DEFAULT_UPDATE_INTERVAL])`.
fn update_intervals_with_default(default_interval: i64) -> Map<String, Value> {
    let mut update: Map<String, Value> = Map::new();
    for (k, v) in [
        ("0", "Default interval"),
        ("-1", "Disable updates"),
        ("15", "15 minutes"),
        ("30", "30 minutes"),
        ("60", "Hourly"),
        ("240", "4 hours"),
        ("720", "12 hours"),
        ("1440", "Daily"),
        ("10080", "Weekly"),
    ] {
        update.insert(k.to_string(), Value::String(v.to_string()));
    }
    update.insert(
        "0".to_string(),
        Value::String(format!(
            "Default interval ({})",
            update_interval_label(default_interval)
        )),
    );
    update
}

/// The minijinja environment holding `editfeeds` (parsed once).
fn editfeeds_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template(
            "editfeeds",
            include_str!("../templates/editfeeds.html.jinja"),
        )
        .expect("valid editfeeds template");
        env
    })
}

#[cfg(test)]
mod debugger_tests {
    use crate::updates::{
        DebugEntry, DEBUG_LEVEL_EXTENDED, DEBUG_LEVEL_NORMAL, DEBUG_LEVEL_VERBOSE, DEBUG_SEPARATOR,
        FeedDebugLog,
    };

    fn log_with(entries: Vec<crate::updates::DebugEntry>) -> FeedDebugLog {
        FeedDebugLog { entries }
    }

    #[test]
    fn empty_log_renders_empty() {
        let out = super::render_debug_log(&log_with(vec![]), 1);
        assert!(out.is_empty());
    }

    #[test]
    fn level_gating_hides_higher_levels() {
        let log = log_with(vec![
            DebugEntry::Log { level: DEBUG_LEVEL_NORMAL, message: "normal".into() },
            DebugEntry::Log { level: DEBUG_LEVEL_VERBOSE, message: "verbose".into() },
            DebugEntry::Log { level: DEBUG_LEVEL_EXTENDED, message: "extended".into() },
        ]);
        // xdebug=1 (LOG_VERBOSE): normal + verbose shown, extended hidden.
        let out = super::render_debug_log(&log, 1);
        assert!(out.contains(">normal</span>"));
        assert!(out.contains(">verbose</span>"));
        assert!(!out.contains("extended"));
        // xdebug=2 (LOG_EXTENDED): everything.
        let out = super::render_debug_log(&log, 2);
        assert!(out.contains(">extended</span>"));
        // xdebug=-1 (LOG_DISABLED): nothing.
        assert!(super::render_debug_log(&log, -1).is_empty());
    }

    #[test]
    fn separator_renders_hr() {
        let log = log_with(vec![DebugEntry::Log {
            level: DEBUG_LEVEL_NORMAL,
            message: DEBUG_SEPARATOR.into(),
        }]);
        let out = super::render_debug_log(&log, 0);
        assert_eq!(out.trim(), "<hr/>");
    }

    #[test]
    fn xml_dump_is_escaped_and_classed() {
        let log = log_with(vec![DebugEntry::Xml("<feed><title>a &amp; b</title></feed>".into())]);
        let out = super::render_debug_log(&log, 0);
        assert!(out.starts_with("<code class='feed-xml'>"));
        assert!(out.contains("&lt;feed&gt;"));
        assert!(!out.contains("<feed>"));
    }

    #[test]
    fn messages_are_html_escaped() {
        let log = log_with(vec![DebugEntry::Log {
            level: DEBUG_LEVEL_NORMAL,
            message: "fetching http://x/<img src=1>".into(),
        }]);
        let out = super::render_debug_log(&log, 0);
        assert!(out.contains("http://x/&lt;img src=1&gt;"));
        assert!(!out.contains("<img"));
    }
}
