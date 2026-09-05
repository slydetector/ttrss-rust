//! Port of the `Feeds` handler's read ops — `view` (the SPA headline list)
//! and `search` (the search-UI params).
//!
//! `backend.php?op=Feeds&method=view` returns `{headlines, headlines-info,
//! runtime-info}`; the per-article objects are the 32-field `ttrss_user_entries`
//! × `ttrss_entries` join with sanitized content, labels, buttons, enclosures,
//! and time fields rendered through [`crate::time`].
//!
//! The `_get_headlines` query is ported branch-for-branch from the PHP
//! (regular feed / category / vfeeds / labels / tags / search), not tuned to
//! the corpus: the same branch conditions assemble the same SQL fragments.

use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::prefs;
use crate::sanitizer;
use crate::session::Sess;
use crate::time::{self, TzCtx};
use crate::AppState;

// `Feeds::*` / `LABEL_BASE_INDEX` (functions.php).
const LABEL_BASE_INDEX: i64 = -1024;
const FEED_ARCHIVED: i64 = 0;
const FEED_STARRED: i64 = -1;
const FEED_PUBLISHED: i64 = -2;
const FEED_FRESH: i64 = -3;
const FEED_ALL: i64 = -4;
const FEED_RECENTLY_READ: i64 = -6;
const FEED_ERROR: i64 = -7;
const CAT_UNCATEGORIZED: i64 = 0;
const CAT_SPECIAL: i64 = -1;
const CAT_LABELS: i64 = -2;
const NEVER_GROUP_FEEDS: &[i64] = &[FEED_RECENTLY_READ, FEED_ARCHIVED];
const NEVER_GROUP_BY_DATE: &[i64] = &[FEED_PUBLISHED, FEED_STARRED, FEED_FRESH];

/// `Labels::feed_to_label_id` = `LABEL_BASE_INDEX - 1 + abs(feed)`.
fn feed_to_label_id(feed: i64) -> i64 {
    LABEL_BASE_INDEX - 1 + feed.abs()
}

/// `Handler::_param_to_bool`.
fn param_to_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let t = s.trim();
            !t.is_empty() && t != "f" && t != "false"
        }
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

/// `Feeds::view`.
pub async fn view(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;

    let feed_raw = extra
        .get("feed")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let method = extra
        .get("m")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let view_mode = extra
        .get("view_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let limit = 30;
    let cat_view = param_to_bool(extra.get("cat"));
    // `$next_unread_feed = $_REQUEST["nuf"] ?? 0` — kept raw (string if sent
    // as a string); only its truthiness matters for the `id` below.
    let next_unread_feed = extra.get("nuf").cloned().unwrap_or(Value::from(0));
    let offset = extra
        .get("skip")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    let order_by = extra
        .get("order_by")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let check_first_id = extra
        .get("fid")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);

    // `if (is_numeric($feed)) $feed = (int) $feed;` (a non-numeric string is a
    // tag name).
    let feed_num: Option<i64> = feed_raw.trim().parse::<i64>().ok();

    // Feed-ownership check (label / feed / category).
    let owned = match feed_num {
        Some(f) if f < LABEL_BASE_INDEX => {
            let n = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2",
            )
            .bind(feed_to_label_id(f))
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            n > 0
        }
        Some(f) if !cat_view && f > 0 => {
            let n = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2",
            )
            .bind(f)
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            n > 0
        }
        Some(f) if cat_view && f > 0 => {
            let n = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM ttrss_feed_categories WHERE id = $1 AND owner_uid = $2",
            )
            .bind(f)
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            n > 0
        }
        _ => true,
    };

    if !owned {
        return generate_error_feed("Feed not found.");
    }

    // `Prefs::set` for the two view prefs (mirrors the PHP side effect).
    pref_set(pool, uid, profile, "_DEFAULT_VIEW_MODE", &view_mode).await;
    pref_set(pool, uid, profile, "_DEFAULT_VIEW_ORDER_BY", &order_by).await;

    // Bump the login timestamp if it's over an hour old.
    if let Some(last_login_update) = data.get_int("last_login_update") {
        let now = chrono::Utc::now().timestamp();
        if now - last_login_update > 3600 {
            let _ = sqlx::query("UPDATE ttrss_users SET last_login = NOW() WHERE id = $1")
                .bind(uid)
                .execute(pool)
                .await;
            sess.with_mut(|s| {
                s.set("last_login_update", ttrss_core::phpser::Value::Int(now));
            });
        }
    }

    if let Some(f) = feed_num {
        if !cat_view && f > 0 {
            let _ = sqlx::query(
                "UPDATE ttrss_feeds SET last_viewed = NOW() WHERE id = $1 AND owner_uid = $2",
            )
            .bind(f)
            .bind(uid)
            .execute(pool)
            .await;
        }
    }

    let (override_order, skip_first_id_check) = order_to_override_query(&order_by);

    // `$_REQUEST["query"] ?? ""` (no active search in the SPA `view` calls).
    let search = extra
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let search_language = extra
        .get("search_language")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let ret = format_headlines_list(
        state,
        sess,
        &feed_raw,
        feed_num,
        &method,
        &view_mode,
        limit,
        cat_view,
        offset,
        &override_order,
        true,
        check_first_id,
        skip_first_id_check,
        &order_by,
        &rows,
        profile,
        &search,
        &search_language,
    )
    .await;

    let headlines_count = ret.count;
    let disable_cache = ret.disable_cache;

    // `$reply['headlines']['id'] = $next_unread_feed ?: $feed;` — `$feed` is
    // the parsed int when numeric, the raw string otherwise (a tag name).
    let nuf_truthy = match &next_unread_feed {
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::Bool(b) => *b,
        _ => false,
    };
    let feed_value = match feed_num {
        Some(f) => Value::from(f),
        None => Value::from(feed_raw.clone()),
    };

    let mut reply = Map::new();
    let mut headlines = ret.headlines;
    headlines
        .insert("id".into(), if nuf_truthy { next_unread_feed } else { feed_value });
    headlines.insert("is_cat".into(), json!(cat_view));
    reply.insert("headlines".into(), Value::Object(headlines));
    reply.insert(
        "headlines-info".into(),
        json!({ "count": headlines_count, "disable_cache": disable_cache }),
    );
    reply.insert(
        "runtime-info".into(),
        crate::rpc::make_runtime_info(state, sess).await,
    );
    Value::Object(reply)
}

/// `Feeds::search` — the search-UI params (not a headline list).
pub async fn search(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let _ = extra;
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;

    // `count(get_hooks(HOOK_SEARCH)) == 0` — none of the loaded plugins in
    // this deployment register HOOK_SEARCH.
    let show_syntax_help = true;

    // `Pref_Feeds::get_ts_languages()` — `ucfirst(cfgname)` per `pg_ts_config`
    // row (natural PG order).
    let cfgs = sqlx::query_scalar::<_, String>("SELECT cfgname FROM pg_ts_config")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut all_languages: Vec<Value> = Vec::new();
    for cfg in &cfgs {
        let mut chars = cfg.chars();
        let s: String = match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        all_languages.push(Value::from(s));
    }

    let default_language = prefs::resolve("DEFAULT_SEARCH_LANGUAGE", &rows, profile).as_str().to_string();

    json!({
        "show_language": true,
        "show_syntax_help": show_syntax_help,
        "all_languages": all_languages,
        "default_language": default_language,
    })
}

/// `Feeds::_generate_error_feed`.
fn generate_error_feed(error: &str) -> Value {
    json!({
        "headlines": {
            "id": FEED_ERROR,
            "is_cat": false,
            "toolbar": "",
            "content": format!("<div class=\"whiteBox\">{error}</div>"),
        },
        "headlines-info": {
            "count": 0,
            "unread": 0,
            "disable_cache": true,
        },
    })
}

/// `Feeds::_order_to_override_query`.
fn order_to_override_query(order: &str) -> (String, bool) {
    let mut query = String::new();
    let mut skip_first_id = false;
    // HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE — none of the loaded plugins in this
    // deployment register it.
    match order {
        "title" => query = "ttrss_entries.title, date_entered, updated".into(),
        "date_reverse" => {
            query = "updated".into();
            skip_first_id = true;
        }
        "feed_dates" => query = "updated DESC".into(),
        _ => {}
    }
    (query, skip_first_id)
}

/// `Prefs::set` (the `_DEFAULT_VIEW_*` upsert) — resolves the current value
/// and writes only on change, exactly like `Prefs::_set`.
async fn pref_set(pool: &sqlx::PgPool, uid: i64, profile: Option<i32>, name: &str, value: &str) {
    let (default, ty) = match prefs::default_for(name) {
        Some(d) => d,
        None => return,
    };
    let _ = ty;
    let new_value = value.trim(); // strip_tags + trim (no tags in view params)

    // current value: (uid, profile) override else default
    let cur: Option<String> = sqlx::query_scalar::<_, String>(
        "SELECT value FROM ttrss_user_prefs2
         WHERE pref_name = $1 AND owner_uid = $2 AND (profile = $3 OR ($3 IS NULL AND profile IS NULL))",
    )
    .bind(name)
    .bind(uid)
    .bind(profile)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let cur = cur.unwrap_or_else(|| default.to_string());
    if cur == new_value {
        return;
    }

    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(pref_name) FROM ttrss_user_prefs2
         WHERE pref_name = $1 AND owner_uid = $2 AND (profile = $3 OR ($3 IS NULL AND profile IS NULL))",
    )
    .bind(name)
    .bind(uid)
    .bind(profile)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    if exists == 0 {
        let _ = sqlx::query(
            "INSERT INTO ttrss_user_prefs2 (pref_name, value, owner_uid, profile) VALUES ($1, $2, $3, $4)",
        )
        .bind(name)
        .bind(new_value)
        .bind(uid)
        .bind(profile)
        .execute(pool)
        .await;
    } else {
        let _ = sqlx::query(
            "UPDATE ttrss_user_prefs2 SET value = $2
             WHERE pref_name = $1 AND owner_uid = $3 AND (profile = $4 OR ($4 IS NULL AND profile IS NULL))",
        )
        .bind(name)
        .bind(new_value)
        .bind(uid)
        .bind(profile)
        .execute(pool)
        .await;
    }
}

// ---------------------------------------------------------------------------
// `_format_headlines_list` + `_get_headlines`
// ---------------------------------------------------------------------------

struct HeadlinesResult {
    headlines: Map<String, Value>,
    count: i64,
    disable_cache: bool,
}

/// `Feeds::_format_headlines_list` (with `_get_headlines` inlined).
async fn format_headlines_list(
    state: &AppState,
    sess: &Sess,
    feed: &str,
    feed_num: Option<i64>,
    _method: &str,
    view_mode: &str,
    limit: i64,
    cat_view: bool,
    offset: i64,
    override_order: &str,
    include_children: bool,
    check_first_id: i64,
    skip_first_id_check: bool,
    order_by: &str,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
    search: &str,
    search_language: &str,
) -> HeadlinesResult {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let pool = state.db.pool();

    // `if ($search) $disable_cache = true;`
    let disable_cache = !search.is_empty();

    let vfeed_group_enabled = prefs::resolve("VFEED_GROUP_BY_FEED", rows, profile).as_bool()
        && !((feed_num.map(|f| NEVER_GROUP_FEEDS.contains(&f)).unwrap_or(false)) && !cat_view);

    // ---- `_get_headlines` ----
    let q = get_headlines(
        pool,
        uid,
        feed,
        feed_num,
        limit,
        view_mode,
        cat_view,
        &search,
        &search_language,
        override_order,
        offset,
        include_children,
        check_first_id,
        skip_first_id_check,
        order_by,
        rows,
        profile,
    )
    .await;

    let HeadlinesQuery {
        article_rows,
        feed_title,
        feed_site_url,
        last_error,
        last_updated,
        first_id,
        is_vfeed,
        query_error_override,
        first_id_changed,
    } = q;

    let tz = TzCtx {
        user_tz: prefs::resolve("USER_TIMEZONE", rows, profile).as_str().to_string(),
        client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
        short_date_format: prefs::resolve("SHORT_DATE_FORMAT", rows, profile).as_str().to_string(),
        long_date_format: prefs::resolve("LONG_DATE_FORMAT", rows, profile).as_str().to_string(),
    };

    // `HOOK_HEADLINE_TOOLBAR_*` — no loaded plugins register them.
    let mut toolbar = Map::new();
    toolbar.insert("site_url".into(), json!(feed_site_url));
    toolbar.insert("title".into(), json!(strip_tags(&feed_title)));
    toolbar.insert("error".into(), json!(last_error));
    toolbar.insert(
        "last_updated".into(),
        json!(time::make_local_datetime(last_updated.as_deref(), false, &tz)),
    );
    toolbar.insert("plugin_menu_items".into(), json!(""));
    toolbar.insert("plugin_buttons".into(), json!(""));

    let mut reply: Map<String, Value> = Map::new();
    reply.insert("first_id".into(), json!(first_id));
    reply.insert("is_vfeed".into(), json!(is_vfeed));
    reply.insert("search_query".into(), json!([search, search_language]));
    reply.insert("vfeed_group_enabled".into(), json!(vfeed_group_enabled));
    reply.insert("toolbar".into(), Value::Object(toolbar));

    let mut content_list: Vec<Value> = Vec::new();
    let mut headlines_count = 0i64;
    let mut rgba_cache: std::collections::HashMap<i64, (i64, i64, i64)> =
        std::collections::HashMap::new();

    // `SELECT` column indices. Non-vfeed rows have 30 columns; vfeed rows add
    // `feed_title` + `favicon_avg_color` after `last_published` (32 cols).
    const I_DATE_ENTERED: usize = 1;
    const I_TITLE: usize = 4;
    const I_UPDATED: usize = 5;
    const I_LABEL_CACHE: usize = 6;
    const I_TAG_CACHE: usize = 7;
    const I_ALWAYS_ENC: usize = 8;
    const I_SITE_URL: usize = 9;
    const I_NOTE: usize = 10;
    const I_NUM_COMMENTS: usize = 11;
    const I_COMMENTS: usize = 12;
    const I_INT_ID: usize = 13;
    const I_UUID: usize = 14;
    const I_LANG: usize = 15;
    const I_HIDE_IMAGES: usize = 16;
    const I_UNREAD: usize = 17;
    const I_FEED_ID: usize = 18;
    const I_MARKED: usize = 19;
    const I_PUBLISHED: usize = 20;
    const I_LINK: usize = 21;
    const I_LAST_READ: usize = 22;
    const I_VF_TITLE: usize = 25;
    const I_VF_FAVICON: usize = 26;
    const I_CONTENT: usize = 25; // non-vfeed
    const I_VF_CONTENT: usize = 27;

    let show_content_preview = prefs::resolve("SHOW_CONTENT_PREVIEW", rows, profile).as_bool();
    let strip_images = prefs::resolve("STRIP_IMAGES", rows, profile).as_bool();
    let bw_limit = data.get_bool("bw_limit").unwrap_or(false);
    let has_sandbox = data.get_bool("hasSandbox").unwrap_or(false);

    for r in &article_rows {
        headlines_count += 1;

        let is_vfeed_row = r.columns().len() >= 32;
        let id: i32 = r.get(0);
        let date_entered: Option<chrono::NaiveDateTime> = r.get(I_DATE_ENTERED);
        let title: String = r.get(I_TITLE);
        let updated: Option<chrono::NaiveDateTime> = r.get(I_UPDATED);
        let label_cache: String = r.get(I_LABEL_CACHE);
        let tag_cache: String = r.get(I_TAG_CACHE);
        // `always_display_enclosures` / `site_url` / `hide_images` come from
        // `ttrss_feeds` — NULL for archived rows (LEFT JOIN miss); PHP emits
        // `null` and treats them as falsy.
        let always_display_enclosures: Option<bool> = r.get(I_ALWAYS_ENC);
        let site_url: Option<String> = r.get(I_SITE_URL);
        let note: Option<String> = r.get(I_NOTE);
        let num_comments: i32 = r.get(I_NUM_COMMENTS);
        let comments: String = r.get(I_COMMENTS);
        let int_id: i32 = r.get(I_INT_ID);
        let uuid: String = r.get(I_UUID);
        let lang: Option<String> = r.get(I_LANG);
        let hide_images: Option<bool> = r.get(I_HIDE_IMAGES);
        let unread: bool = r.get(I_UNREAD);
        let feed_id_db: Option<i32> = r.get(I_FEED_ID);
        let marked: bool = r.get(I_MARKED);
        let published: bool = r.get(I_PUBLISHED);
        let link: String = r.get(I_LINK);
        let last_read: Option<chrono::NaiveDateTime> = r.get(I_LAST_READ);
        let vfeed_title: Option<String> =
            if is_vfeed_row { r.get::<Option<String>, _>(I_VF_TITLE) } else { None };
        let favicon_avg_color: Option<String> =
            if is_vfeed_row { r.get::<Option<String>, _>(I_VF_FAVICON) } else { None };
        let content_raw: String = r.get(if is_vfeed_row { I_VF_CONTENT } else { I_CONTENT });
        let author: String = r.get(if is_vfeed_row { 28 } else { 26 });
        let score: i32 = r.get(if is_vfeed_row { 29 } else { 27 });
        let num_labels: i64 = r.get(if is_vfeed_row { 30 } else { 28 });
        let num_enclosures: i64 = r.get(if is_vfeed_row { 31 } else { 29 });

        let updated_str = updated.as_ref().map(|dt| time::naive_to_pg_text(dt));
        let date_entered_str = date_entered.as_ref().map(|dt| time::naive_to_pg_text(dt));
        let last_read_str = last_read.as_ref().map(|dt| time::naive_to_pg_text(dt));

        // `content_preview` — first key added (pre-sanitize raw content).
        let content_preview = if show_content_preview {
            format!(
                "&mdash; {}",
                truncate_string(&strip_tags(&content_raw), 250)
            )
        } else {
            String::new()
        };

        // `if ($line['feed_id'] === null)` — normalize the archived feed.
        let feed_id: i64 = feed_id_db.map(|f| f as i64).unwrap_or(FEED_ARCHIVED);
        let archived = feed_id_db.is_none();
        let feed_title_out = if let Some(t) = &vfeed_title {
            Some(t.clone())
        } else if archived {
            Some("Archived articles".to_string())
        } else {
            None
        };

        // labels (`Article::_get_labels` when the cache is empty)
        let labels: Value = if num_labels > 0 {
            let cache = label_cache.trim();
            if cache.is_empty() {
                get_labels_fallback(pool, uid, id).await
            } else {
                match serde_json::from_str::<Value>(cache) {
                    Ok(v) => {
                        if let Some(obj) = v.as_object() {
                            if obj.get("no-labels").and_then(|x| x.as_i64()) == Some(1) {
                                Value::Array(Vec::new())
                            } else {
                                v
                            }
                        } else {
                            v
                        }
                    }
                    // Invalid cache: PHP keeps `$labels = false`.
                    Err(_) => Value::Bool(false),
                }
            }
        } else {
            Value::Array(Vec::new())
        };

        // `Sanitizer::sanitize(content, hide_images, null, site_url, null, id)`
        let content = sanitizer::sanitize(
            &content_raw,
            hide_images.unwrap_or(false),
            strip_images,
            has_sandbox,
            bw_limit,
            site_url.as_deref().filter(|s| !s.is_empty()),
            None,
        );

        let tags = if tag_cache.is_empty() {
            Vec::new()
        } else {
            tag_cache.split(',').map(|s| s.to_string()).collect::<Vec<_>>()
        };

        // `Article::_format_enclosures` (sql_bool_to_bool: NULL → false)
        let enclosures = if num_enclosures > 0 {
            format_enclosures(
                pool,
                id,
                always_display_enclosures.unwrap_or(false),
                &content,
                hide_images.unwrap_or(false),
                bw_limit,
                strip_images,
            )
            .await
        } else {
            json!({ "formatted": "", "entries": [] })
        };

        // feed headline background color (per-feed-id cache, like PHP's
        // `$rgba_cache`)
        if !rgba_cache.contains_key(&feed_id) {
            let fav_color = favicon_avg_color.as_deref().unwrap_or("");
            let rgb = if !fav_color.is_empty() && fav_color != "fail" {
                color_unpack(fav_color)
            } else {
                color_of(feed_title_out.as_deref().unwrap_or(""))
            };
            rgba_cache.insert(feed_id, rgb);
        }
        let (rr, gg, bb) = &rgba_cache[&feed_id];

        let is_shared = !uuid.is_empty();
        let lang: Option<String> = if is_shared {
            Some("shared".to_string())
        } else {
            lang
        };

        // Key order: the SELECT columns (minus the `unset` ones), then the
        // keys the PHP loop appends (`content_preview`, archived
        // `feed_title`, `labels`, `feed_title ??= ''`, buttons, …).
        let mut line = Map::new();
        line.insert("id".into(), json!(id));
        line.insert("title".into(), json!(title));
        line.insert(
            "updated".into(),
            json!(time::make_local_datetime(
                updated_str.as_deref(),
                true,
                &tz
            )),
        );
        line.insert("always_display_enclosures".into(), json!(always_display_enclosures));
        line.insert("site_url".into(), json!(site_url));
        line.insert("note".into(), json!(note));
        line.insert("num_comments".into(), json!(num_comments));
        line.insert("comments".into(), json!(comments));
        line.insert("int_id".into(), json!(int_id));
        line.insert("lang".into(), json!(lang));
        line.insert("hide_images".into(), json!(hide_images));
        line.insert("unread".into(), json!(unread));
        line.insert("feed_id".into(), json!(feed_id));
        line.insert("marked".into(), json!(marked));
        line.insert("published".into(), json!(published));
        line.insert("link".into(), json!(link));
        line.insert("last_read".into(), json!(last_read_str));
        if let Some(t) = &vfeed_title {
            line.insert("feed_title".into(), json!(t));
        }
        line.insert("content".into(), json!(content));
        line.insert("author".into(), json!(author));
        line.insert("score".into(), json!(score));
        line.insert("num_labels".into(), json!(num_labels));
        line.insert("content_preview".into(), json!(content_preview));
        if archived && vfeed_title.is_none() {
            line.insert("feed_title".into(), json!("Archived articles"));
        }
        line.insert("labels".into(), labels);
        if vfeed_title.is_none() && !archived {
            // `$line["feed_title"] ??= ""`
            line.insert("feed_title".into(), json!(""));
        }
        line.insert("buttons_left".into(), json!(""));
        line.insert("buttons".into(), json!(article_buttons(id, int_id, is_shared)));
        line.insert("enclosures".into(), enclosures);
        line.insert(
            "updated_long".into(),
            json!(time::make_local_datetime(updated_str.as_deref(), false, &tz)),
        );
        line.insert(
            "imported".into(),
            json!(format!(
                "Imported at {}",
                time::make_local_datetime(date_entered_str.as_deref(), false, &tz)
            )),
        );
        line.insert("tags".into(), Value::Array(tags.iter().map(|s| Value::from(s.as_str())).collect()));
        line.insert("has_icon".into(), json!(has_icon(&state.config.feed_icon_dir, feed_id)));
        line.insert(
            "feed_bg_color".into(),
            json!(format!("rgba({rr},{gg},{bb},0.3)")),
        );

        content_list.push(Value::Object(line));
    }

    if headlines_count == 0 {
        // `if ($result instanceof PDOStatement)` — the empty-message block.
        let message = if !query_error_override.is_empty() {
            query_error_override.clone()
        } else {
            match view_mode {
                "unread" => "No unread articles found to display.".to_string(),
                "updated" => "No updated articles found to display.".to_string(),
                "marked" => "No starred articles found to display.".to_string(),
                _ => {
                    if feed_num.map(|f| f < LABEL_BASE_INDEX).unwrap_or(false) {
                        "No articles found to display. You can assign articles to labels manually from article header context menu (applies to all selected articles) or use a filter.".to_string()
                    } else {
                        "No articles found to display.".to_string()
                    }
                }
            }
        };
        if offset == 0 && !message.is_empty() {
            let last_updated: Option<String> =
                sqlx::query_scalar::<_, String>(
                    "SELECT SUBSTRING_FOR_DATE(MAX(last_updated), 1, 19) AS last_updated
                     FROM ttrss_feeds WHERE owner_uid = $1",
                )
                .bind(uid)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
            let num_errors = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(id) FROM ttrss_feeds
                 WHERE last_error != '' AND owner_uid = $1 AND update_interval >= 0",
            )
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0);

            let mut msg = String::from("<div class=\"whiteBox\">");
            if query_error_override.is_empty() {
                msg.push_str(&message);
            } else {
                msg.push_str(&format!("<strong>{message}</strong>"));
            }
            msg.push_str("<p><span class=\"text-muted\">");
            msg.push_str(&format!(
                "Feeds last updated at {}",
                time::make_local_datetime(last_updated.as_deref(), false, &tz)
            ));
            if num_errors > 0 {
                msg.push_str(
                    "<br/><a class=\"text-muted\" href=\"#\" onclick=\"CommonDialogs.showFeedsWithErrors(); return false\">Some feeds have update errors (click for details)</a>",
                );
            }
            msg.push_str("</span></p></div>");
            reply.insert("content".into(), Value::from(msg));
        }
    } else {
        reply.insert("content".into(), Value::Array(content_list));
    }

    if first_id_changed {
        reply.insert("first_id_changed".into(), json!(true));
    }

    HeadlinesResult {
        headlines: reply,
        count: headlines_count,
        disable_cache,
    }
}

/// `Article::_get_labels` fallback (only when `label_cache` is empty).
async fn get_labels_fallback(pool: &sqlx::PgPool, uid: i64, id: i32) -> Value {
    let rows = sqlx::query(
        "SELECT DISTINCT label_id, caption, fg_color, bg_color
         FROM ttrss_labels2, ttrss_user_labels2
         WHERE id = label_id AND article_id = $1 AND owner_uid = $2
         ORDER BY caption",
    )
    .bind(id)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();
    for r in &rows {
        let label_id = r.get::<i32, _>(0) as i64;
        out.push(json!([
            LABEL_BASE_INDEX - 1 - label_id.abs(),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
            r.get::<String, _>(3),
        ]));
    }
    Value::Array(out)
}

/// The three HOOK_ARTICLE_BUTTON plugins in load order (af_psql_trgm, note,
/// share). The PHP DOM round-trip normalizes the markup (attribute order
/// preserved, single quotes become double quotes, `data-plugin-name` appended
/// last); the result strings below are the oracle's exact output.
fn article_buttons(id: i32, int_id: i32, is_shared: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<i style=\"cursor : pointer\" class=\"material-icons\" onclick=\"Plugins.Psql_Trgm.showRelated({id})\" title=\"Show related articles\" data-plugin-name=\"Af_Psql_Trgm\">bookmark_outline</i>"
    ));
    out.push_str(&format!(
        "<i class=\"material-icons\" onclick=\"Plugins.Note.edit({id})\" style=\"cursor : pointer\" title=\"Edit article note\" data-plugin-name=\"Note\">note</i>"
    ));
    // `is-shared` class only when the article has a share uuid.
    let icon_class = if is_shared { "is-shared" } else { "" };
    out.push_str(&format!(
        "<i class=\"material-icons icon-share share-icon-{int_id} {icon_class}\" style=\"cursor : pointer\" onclick=\"Plugins.Share.shareArticle({int_id})\" title=\"Share by URL\" data-plugin-name=\"Share\">link</i>"
    ));
    out
}

/// `Article::_format_enclosures`.
async fn format_enclosures(
    pool: &sqlx::PgPool,
    id: i32,
    always_display_enclosures: bool,
    article_content: &str,
    hide_images: bool,
    bw_limit: bool,
    strip_images: bool,
) -> Value {
    let encs = sqlx::query(
        "SELECT id, content_url, content_type, title, duration, width, height, post_id
         FROM ttrss_enclosures WHERE post_id = $1",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let can_inline = !bw_limit
        && !strip_images
        && (always_display_enclosures || !contains_img(article_content));
    let inline_text_only = hide_images && can_inline;

    let mut entries: Vec<Value> = Vec::new();
    let mut formatted = String::new();
    for r in &encs {
        let content_url: String = r.get(1);
        let vid = url_to_youtube_vid(&content_url);
        if let Some(vid) = vid {
            // HOOK_RENDER_ENCLOSURE (af_youtube_embed)
            formatted.push_str(&format!(
                "<div class=\"embed-responsive\"><iframe class=\"youtube-player\" type=\"text/html\" width=\"640\" height=\"385\" title=\"YouTube video player\" src=\"https://www.youtube-nocookie.com/embed/{vid}\" referrerpolicy=\"strict-origin-when-cross-origin\" allow=\"accelerometer; autoplay; clipboard-write; encrypted-media; fullscreen *; gyroscope; picture-in-picture; web-share\" sandbox=\"allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox\" frameborder=\"0\"></iframe></div>"
            ));
            continue;
        }
        let filename = basename(&content_url);
        entries.push(json!({
            "id": r.get::<i32, _>(0),
            "content_url": content_url,
            "content_type": r.get::<String, _>(2),
            "title": r.get::<String, _>(3),
            "duration": r.get::<String, _>(4),
            "width": r.get::<i32, _>(5),
            "height": r.get::<i32, _>(6),
            "post_id": r.get::<i32, _>(7),
            "filename": filename,
        }));
    }

    json!({
        "formatted": formatted,
        "entries": entries,
        "can_inline": can_inline,
        "inline_text_only": inline_text_only,
    })
}

/// `preg_match("/<img/i", $content)`.
fn contains_img(content: &str) -> bool {
    content.to_lowercase().contains("<img")
}

/// `basename($url)` — the last path segment (PHP semantics).
fn basename(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let no_slash = url.rfind('/').map(|i| &url[i + 1..]).unwrap_or(url);
    no_slash.to_string()
}

/// `UrlHelper::url_to_youtube_vid`.
fn url_to_youtube_vid(url: &str) -> Option<String> {
    let url = url.replace("youtube.com", "youtube-nocookie.com");
    let patterns = [
        r"//www\.youtube-nocookie\.com/v/([\w-]+)",
        r"//www\.youtube-nocookie\.com/embed/([\w-]+)",
        r"//www\.youtube-nocookie\.com/watch\?v=([\w-]+)",
        r"//youtu\.be/([\w-]+)",
    ];
    for p in patterns {
        if let Some(re) = regex::Regex::new(p).ok() {
            if let Some(caps) = re.captures(&url) {
                return Some(caps[1].to_string());
            }
        }
    }
    None
}

/// `Feeds::_has_icon`.
fn has_icon(icon_dir: &std::path::Path, feed_id: i64) -> bool {
    if feed_id <= 0 {
        return false;
    }
    icon_dir.join(feed_id.to_string()).is_file()
}

/// `Feeds::_color_of`.
fn color_of(name: &str) -> (i64, i64, i64) {
    const COLORMAP: [(&str, (i64, i64, i64)); 15] = [
        ("#1cd7d7", (0x1c, 0xd7, 0xd7)),
        ("#d91111", (0xd9, 0x11, 0x11)),
        ("#1212d7", (0x12, 0x12, 0xd7)),
        ("#8e16e5", (0x8e, 0x16, 0xe5)),
        ("#7b7b7b", (0x7b, 0x7b, 0x7b)),
        ("#39f110", (0x39, 0xf1, 0x10)),
        ("#0bbea6", (0x0b, 0xbe, 0xa6)),
        ("#ec0e0e", (0xec, 0x0e, 0x0e)),
        ("#1534f2", (0x15, 0x34, 0xf2)),
        ("#b9e416", (0xb9, 0xe4, 0x16)),
        ("#479af2", (0x47, 0x9a, 0xf2)),
        ("#f36b14", (0xf3, 0x6b, 0x14)),
        ("#10c7e9", (0x10, 0xc7, 0xe9)),
        ("#1e8fe7", (0x1e, 0x8f, 0xe7)),
        ("#e22727", (0xe2, 0x27, 0x27)),
    ];
    let mut sum = 0i64;
    for b in name.bytes() {
        sum += b as i64;
    }
    let idx = (sum % COLORMAP.len() as i64) as usize;
    COLORMAP[idx].1
}

/// `Colors::_color_unpack`.
fn color_unpack(hex: &str) -> (i64, i64, i64) {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let hex = if hex.len() == 4 {
        // #rgb → #rrggbb
        format!(
            "{}{}{}{}{}{}",
            hex.as_bytes()[1] as char,
            hex.as_bytes()[1] as char,
            hex.as_bytes()[2] as char,
            hex.as_bytes()[2] as char,
            hex.as_bytes()[3] as char,
            hex.as_bytes()[3] as char
        )
    } else {
        hex.to_string()
    };
    let c = u32::from_str_radix(&hex, 16).unwrap_or(0);
    (((c >> 16) & 0xFF) as i64, ((c >> 8) & 0xFF) as i64, (c & 0xFF) as i64)
}

/// `strip_tags` (the M2 usage: drop HTML tags/comments, keep text + entities).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(c) = after.chars().next() {
            if c == '!' && after.starts_with("<!--") {
                match after[4..].find("-->") {
                    Some(j) => rest = &after[4 + j + 3..],
                    None => return out,
                }
                continue;
            }
            if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?' {
                match after.find('>') {
                    Some(j) => {
                        rest = &after[j + 1..];
                        continue;
                    }
                    // non-terminated tag: PHP strips to the end of the string
                    None => return out,
                }
            }
        }
        out.push('<');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// `truncate_string($str, $max_len)` — `&hellip;` suffix (char-based).
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let cut: String = s.chars().take(max_len).collect();
        format!("{cut}&hellip;")
    } else {
        s.to_string()
    }
}

// ---- `_get_headlines` ------------------------------------------------------

struct HeadlinesQuery {
    article_rows: Vec<sqlx::postgres::PgRow>,
    feed_title: String,
    feed_site_url: String,
    last_error: String,
    last_updated: Option<String>,
    first_id: i64,
    is_vfeed: bool,
    query_error_override: String,
    first_id_changed: bool,
}

/// `Feeds::_get_headlines` — the dynamic headline query, ported branch-for-
/// branch from the PHP.
async fn get_headlines(
    pool: &sqlx::PgPool,
    uid: i64,
    feed: &str,
    feed_num: Option<i64>,
    limit: i64,
    view_mode: &str,
    cat_view: bool,
    search: &str,
    _search_language: &str,
    override_order: &str,
    offset: i64,
    include_children: bool,
    check_first_id: i64,
    skip_first_id_check: bool,
    _order_by: &str,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
) -> HeadlinesQuery {
    // The branch logic below mutates `$override_order` (starred / published /
    // recently-read get a default order unless the caller supplied one).
    let mut override_order = override_order.to_string();

    let mut query_strategy_part = String::new();
    let mut vfeed_query_part = String::new();
    let mut ext_tables_part = String::new();
    let mut allow_archived = false;
    let mut ignore_vfeed_group = false;
    let mut first_id_query_strategy_part = String::new();

    let fresh_max_age = prefs::resolve("FRESH_ARTICLE_MAX_AGE", rows, profile).as_int();
    let recently_read_max_age = prefs::resolve("RECENTLY_READ_MAX_AGE", rows, profile).as_int();

    // $view_query_part
    let mut view_query_part = String::new();
    if view_mode == "adaptive" {
        // not exercised in the corpus (view_mode="" in all steps)
        let _ = (feed_num, cat_view, include_children);
    }
    view_query_part = match view_mode {
        "marked" => " marked = true AND ".into(),
        "has_note" => " (note IS NOT NULL AND note != '') AND ".into(),
        "published" => " published = true AND ".into(),
        "unread" if feed_num != Some(FEED_RECENTLY_READ) => " unread = true AND ".into(),
        _ => view_query_part,
    };

    let limit_query_part = if limit > 0 { format!("LIMIT {limit}") } else { String::new() };
    let offset_query_part = if limit_query_part.is_empty() {
        String::new()
    } else {
        format!("OFFSET {offset}")
    };

    // query strategy per branch
    match feed_num {
        None => {
            // browsing by tag
            query_strategy_part = "true".into();
            vfeed_query_part =
                "(SELECT title FROM ttrss_feeds WHERE\n\t\t\t\t\tid = feed_id) as feed_title,".into();
        }
        Some(f) if f > 0 => {
            if cat_view {
                if include_children {
                    let mut subcats: Vec<i64> = child_cats(pool, uid, f).await;
                    subcats.push(f);
                    let list = subcats.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",");
                    query_strategy_part = format!("cat_id IN ({list})");
                } else {
                    query_strategy_part = format!("cat_id = {f}");
                }
                vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
            } else {
                query_strategy_part = format!("feed_id = {f}");
            }
        }
        Some(f) if f == FEED_ARCHIVED && !cat_view => {
            query_strategy_part = "feed_id IS NULL".into();
            allow_archived = true;
        }
        Some(f) if f == CAT_UNCATEGORIZED && cat_view => {
            query_strategy_part = "cat_id IS NULL AND feed_id IS NOT NULL".into();
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
        }
        Some(f) if f == FEED_STARRED => {
            query_strategy_part = "marked = true".into();
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
            allow_archived = true;
            if override_order.is_empty() {
                override_order =
                    "last_marked DESC, date_entered DESC, updated DESC".into();
            }
        }
        Some(f) if f == FEED_PUBLISHED || f == CAT_LABELS => {
            if !cat_view {
                query_strategy_part = "published = true".into();
                vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
                allow_archived = true;
                if override_order.is_empty() {
                    override_order =
                        "last_published DESC, date_entered DESC, updated DESC".into();
                }
            } else {
                vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
                ext_tables_part = "ttrss_labels2,ttrss_user_labels2,".into();
                query_strategy_part = "ttrss_labels2.id = ttrss_user_labels2.label_id AND
\t\t\t\t\tttrss_user_labels2.article_id = ref_id".into();
            }
        }
        Some(f) if f == FEED_RECENTLY_READ => {
            query_strategy_part = format!(
                "unread = false AND last_read IS NOT NULL AND
\t\t\t\tlast_read > NOW() - INTERVAL '{recently_read_max_age} hour'"
            );
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
            allow_archived = true;
            ignore_vfeed_group = true;
            if override_order.is_empty() {
                override_order = "last_read DESC".into();
            }
        }
        Some(f) if f == FEED_FRESH => {
            query_strategy_part = format!(
                "unread = true AND score >= 0 AND
\t\t\t\tdate_entered > NOW() - INTERVAL '{fresh_max_age} hour'"
            );
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
        }
        Some(f) if f == FEED_ALL => {
            allow_archived = true;
            query_strategy_part = "true".into();
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
        }
        Some(f) if f <= LABEL_BASE_INDEX => {
            let label_id = feed_to_label_id(f);
            query_strategy_part = format!(
                "label_id = {label_id} AND
\t\t\t\t\tttrss_labels2.id = ttrss_user_labels2.label_id AND
\t\t\t\t\tttrss_user_labels2.article_id = ref_id"
            );
            vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
            ext_tables_part = "ttrss_labels2,ttrss_user_labels2,".into();
            allow_archived = true;
        }
        _ => {
            query_strategy_part = "true".into();
        }
    }

    // `$order_by = $override_order ?: "score DESC, date_entered DESC, updated DESC"`
    let mut order_by = if override_order.is_empty() {
        "score DESC, date_entered DESC, updated DESC".to_string()
    } else {
        override_order.clone()
    };

    // feed title / site url / error / last updated
    let (feed_title, feed_site_url, last_error, last_updated) =
        if !search.is_empty() {
            (format!("Search results: {search}"), String::new(), String::new(), None)
        } else if cat_view {
            (cat_title(pool, uid, feed_num.unwrap_or(0)).await, String::new(), String::new(), None)
        } else if let Some(f) = feed_num {
            if f > 0 {
                let row = sqlx::query(
                    "SELECT title, site_url, last_error, last_updated
                     FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2",
                )
                .bind(f)
                .bind(uid)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
                if let Some(r) = row {
                    (
                        r.get::<String, _>(0),
                        r.get::<String, _>(1),
                        r.get::<String, _>(2),
                        r.get::<Option<chrono::NaiveDateTime>, _>(3)
                            .map(|dt| time::naive_to_pg_text(&dt)),
                    )
                } else {
                    (format!("Unknown feed ({f})"), String::new(), String::new(), None)
                }
            } else {
                (
                    feed_title_of(f, pool, uid).await,
                    String::new(),
                    String::new(),
                    None,
                )
            }
        } else {
            // tag browsing: the tag name itself
            (feed.to_string(), String::new(), String::new(), None)
        };

    // ---- assemble the main query (numeric feed path) ----
    let yyiw_qpart = "to_char(date_entered, 'IYYY-IW') AS yyiw";
    let content_query_part = "content, ";

    let mut first_id = 0i64;
    let first_id_changed = false;
    let mut article_rows: Vec<sqlx::postgres::PgRow> = Vec::new();

    if let Some(feed) = feed_num {
        // vfeed group ordering (VFEED_GROUP_BY_FEED is false in this
        // deployment; ported for parity)
        if !vfeed_query_part.is_empty()
            && !ignore_vfeed_group
            && prefs::resolve("VFEED_GROUP_BY_FEED", rows, profile).as_bool()
        {
            let mut yyiw_order_qpart = String::new();
            if !((NEVER_GROUP_BY_DATE.contains(&feed)) && !cat_view) {
                let yyiw_desc = if order_by == "date_reverse" { "" } else { "desc" };
                yyiw_order_qpart = format!("yyiw {yyiw_desc}, ");
            }
            let tail = if override_order.is_empty() {
                order_by.clone()
            } else {
                override_order.clone()
            };
            order_by = format!("{yyiw_order_qpart} ttrss_feeds.title, {tail}");
        }

        let (from_qpart, feed_check_qpart) = if !allow_archived {
            (
                format!(
                    "{ext_tables_part}ttrss_entries LEFT JOIN ttrss_user_entries ON (ref_id = ttrss_entries.id), ttrss_feeds"
                ),
                "ttrss_user_entries.feed_id = ttrss_feeds.id AND".to_string(),
            )
        } else {
            (
                format!(
                    "{ext_tables_part}ttrss_entries LEFT JOIN ttrss_user_entries ON (ref_id = ttrss_entries.id)\n\t\t\t\t\tLEFT JOIN ttrss_feeds ON (feed_id = ttrss_feeds.id)"
                ),
                String::new(),
            )
        };

        if !vfeed_query_part.is_empty() {
            vfeed_query_part.push_str("favicon_avg_color,");
        }

        first_id_query_strategy_part = query_strategy_part.clone();
        if feed == FEED_FRESH {
            first_id_query_strategy_part = "true".into();
        }

        let distinct_columns = order_by.to_lowercase().replace("desc", "");
        let mut distinct_qpart = format!("DISTINCT ON (id, {distinct_columns})");
        if prefs::resolve("HEADLINES_NO_DISTINCT", rows, profile).as_bool()
            && !(feed == CAT_LABELS && cat_view)
        {
            distinct_qpart = String::new();
        }

        // first-id check
        if search.is_empty() && !skip_first_id_check {
            let query = format!(
                "SELECT
\t\t\t\t\t\tttrss_entries.id,
\t\t\t\t\t\tdate_entered,
\t\t\t\t\t\t{yyiw_qpart},
\t\t\t\t\t\tguid,
\t\t\t\t\t\tttrss_entries.title,
\t\t\t\t\t\tttrss_feeds.title,
\t\t\t\t\t\tupdated,
\t\t\t\t\t\tscore,
\t\t\t\t\t\tmarked,
\t\t\t\t\t\tpublished,
\t\t\t\t\t\tlast_marked,
\t\t\t\t\t\tlast_published,
\t\t\t\t\t\tlast_read
\t\t\t\tFROM
\t\t\t\t\t{from_qpart}
\t\t\t\tWHERE
\t\t\t\t{feed_check_qpart}
\t\t\t\tttrss_user_entries.owner_uid = '{uid}' AND
\t\t\t\tdate_entered >= NOW() - INTERVAL '1 hour' AND
\t\t\t\t{first_id_query_strategy_part} ORDER BY {order_by} LIMIT 1"
            );
            if let Ok(row) = sqlx::query(&query).fetch_optional(pool).await {
                if let Some(r) = row {
                    first_id = r.get::<i32, _>(0) as i64;
                    if offset > 0 && first_id != 0 && check_first_id != 0 && first_id != check_first_id {
                        return HeadlinesQuery {
                            article_rows: Vec::new(),
                            feed_title,
                            feed_site_url,
                            last_error,
                            last_updated,
                            first_id,
                            is_vfeed: !vfeed_query_part.is_empty(),
                            query_error_override: String::new(),
                            first_id_changed: true,
                        };
                    }
                }
            }
        }

        let query = format!(
            "SELECT {distinct_qpart}
\t\t\t\t\t\tttrss_entries.id AS id,
\t\t\t\t\t\tdate_entered,
\t\t\t\t\t\t{yyiw_qpart},
\t\t\t\t\t\tguid,
\t\t\t\t\t\tttrss_entries.title,
\t\t\t\t\t\tupdated,
\t\t\t\t\t\tlabel_cache,
\t\t\t\t\t\ttag_cache,
\t\t\t\t\t\talways_display_enclosures,
\t\t\t\t\t\tsite_url,
\t\t\t\t\t\tnote,
\t\t\t\t\t\tnum_comments,
\t\t\t\t\t\tcomments,
\t\t\t\t\t\tint_id,
\t\t\t\t\t\tuuid,
\t\t\t\t\t\tlang,
\t\t\t\t\t\thide_images,
\t\t\t\t\t\tunread,feed_id,marked,published,link,last_read,
\t\t\t\t\t\tlast_marked, last_published,
\t\t\t\t\t\t{vfeed_query_part}
\t\t\t\t\t\t{content_query_part}
\t\t\t\t\t\tauthor,score,
\t\t\t\t\t\t(SELECT count(label_id) FROM ttrss_user_labels2 WHERE article_id = ttrss_entries.id) AS num_labels,
\t\t\t\t\t\t(SELECT count(id) FROM ttrss_enclosures WHERE post_id = ttrss_entries.id) AS num_enclosures
\t\t\t\tFROM
\t\t\t\t\t{from_qpart}
\t\t\t\tWHERE
\t\t\t\t{feed_check_qpart}
\t\t\t\tttrss_user_entries.owner_uid = '{uid}' AND
\t\t\t\t{view_query_part}
\t\t\t\t{query_strategy_part} ORDER BY {order_by}
\t\t\t\t{limit_query_part} {offset_query_part}"
        );

        article_rows = sqlx::query(&query)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    } else {
        // browsing by tag (not exercised in the corpus; ported for parity)
        let distinct_columns = order_by.to_lowercase().replace("desc", "");
        let distinct_qpart = if prefs::resolve("HEADLINES_NO_DISTINCT", rows, profile).as_bool() {
            String::new()
        } else {
            format!("DISTINCT ON (id, {distinct_columns})")
        };
        let query = format!(
            "SELECT {distinct_qpart}
\t\t\t\t\t\t\t\tttrss_entries.id AS id,
\t\t\t\t\t\t\t\tdate_entered,
\t\t\t\t\t\t\t\t{yyiw_qpart},
\t\t\t\t\t\t\t\tguid,
\t\t\t\t\t\t\t\tttrss_entries.title,
\t\t\t\t\t\t\t\tupdated,
\t\t\t\t\t\t\t\tlabel_cache,
\t\t\t\t\t\t\t\ttag_cache,
\t\t\t\t\t\t\t\talways_display_enclosures,
\t\t\t\t\t\t\t\tsite_url,
\t\t\t\t\t\t\t\tnote,
\t\t\t\t\t\t\t\tnum_comments,
\t\t\t\t\t\t\t\tcomments,
\t\t\t\t\t\t\t\tint_id,
\t\t\t\t\t\t\t\tuuid,
\t\t\t\t\t\t\t\tlang,
\t\t\t\t\t\t\t\thide_images,
\t\t\t\t\t\t\t\tunread,feed_id,marked,published,link,last_read,
\t\t\t\t\t\t\t\tlast_marked, last_published,
\t\t\t\t\t\t\t\t{vfeed_query_part}
\t\t\t\t\t\t\t\t{content_query_part}
\t\t\t\t\t\t\t\tauthor, score,
\t\t\t\t\t\t\t\t(SELECT count(label_id) FROM ttrss_user_labels2 WHERE article_id = ttrss_entries.id) AS num_labels,
\t\t\t\t\t\t\t\t(SELECT count(id) FROM ttrss_enclosures WHERE post_id = ttrss_entries.id) AS num_enclosures
\t\t\t\t\t\tFROM ttrss_entries,
\t\t\t\t\t\t\tttrss_user_entries LEFT JOIN ttrss_feeds ON (ttrss_feeds.id = ttrss_user_entries.feed_id),
\t\t\t\t\t\t\tttrss_tags
\t\t\t\t\t\tWHERE
\t\t\t\t\t\t\tref_id = ttrss_entries.id AND
\t\t\t\t\t\t\tttrss_user_entries.owner_uid = '{uid}' AND
\t\t\t\t\t\t\tpost_int_id = int_id AND
\t\t\t\t\t\t\ttag_name = '{feed}' AND
\t\t\t\t\t\t\t{view_query_part}
\t\t\t\t\t\t\t{query_strategy_part} ORDER BY {order_by}
\t\t\t\t\t\t\t{limit_query_part} {offset_query_part}"
        );
        article_rows = sqlx::query(&query)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    }

    HeadlinesQuery {
        article_rows,
        feed_title,
        feed_site_url,
        last_error,
        last_updated,
        first_id,
        is_vfeed: !vfeed_query_part.is_empty(),
        query_error_override: String::new(),
        first_id_changed,
    }
}

/// `Feeds::_get_child_cats` (natural PG order).
async fn child_cats(pool: &sqlx::PgPool, uid: i64, cat: i64) -> Vec<i64> {
    let rows = sqlx::query(
        "SELECT id FROM ttrss_feed_categories WHERE (parent_cat = $1 AND owner_uid = $2)",
    )
    .bind(cat)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter().map(|r| r.get::<i32, _>(0) as i64).collect()
}

/// `Feeds::_get_cat_title`.
async fn cat_title(pool: &sqlx::PgPool, uid: i64, cat_id: i64) -> String {
    match cat_id {
        CAT_UNCATEGORIZED => "Uncategorized".into(),
        CAT_SPECIAL => "Special".into(),
        CAT_LABELS => "Labels".into(),
        _ => {
            let row: Option<String> = sqlx::query_scalar::<_, String>(
                "SELECT title FROM ttrss_feed_categories WHERE id = $1 AND owner_uid = $2",
            )
            .bind(cat_id)
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            row.unwrap_or_else(|| "UNKNOWN".into())
        }
    }
}

/// `Feeds::_get_title` (the non-cat branch).
async fn feed_title_of(feed: i64, pool: &sqlx::PgPool, uid: i64) -> String {
    match feed {
        FEED_STARRED => "Starred articles".into(),
        FEED_PUBLISHED => "Published articles".into(),
        FEED_FRESH => "Fresh articles".into(),
        FEED_ALL => "All articles".into(),
        FEED_ARCHIVED => "Archived articles".into(),
        FEED_RECENTLY_READ => "Recently read".into(),
        f if f < LABEL_BASE_INDEX => {
            let label_id = feed_to_label_id(f);
            let caption: Option<String> = sqlx::query_scalar::<_, String>(
                "SELECT caption FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2",
            )
            .bind(label_id)
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            caption.unwrap_or_else(|| format!("Unknown label ({label_id})"))
        }
        f if f > 0 => {
            let title: Option<String> = sqlx::query_scalar::<_, String>(
                "SELECT title FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2",
            )
            .bind(f)
            .bind(uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            title.unwrap_or_else(|| format!("Unknown feed ({f})"))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_basics() {
        assert_eq!(strip_tags("<p>hello</p>"), "hello");
        assert_eq!(strip_tags("a <b>bold</b> &amp; c"), "a bold &amp; c");
        assert_eq!(strip_tags("<!-- comment -->text"), "text");
        assert_eq!(strip_tags("a < 5"), "a < 5");
        assert_eq!(strip_tags("<div>multi\n  line</div>"), "multi\n  line");
    }

    #[test]
    fn truncate_string_chars() {
        // test_truncate_string_no_truncation
        assert_eq!(truncate_string("hello", 10), "hello");
        // test_truncate_string_exact_length
        assert_eq!(truncate_string("hello", 5), "hello");
        // test_truncate_string_shortens
        assert_eq!(truncate_string("hello world", 5), "hello&hellip;");
        // test_truncate_string_empty_string
        assert_eq!(truncate_string("", 10), "");
        // test_truncate_string_max_len_zero
        assert_eq!(truncate_string("hello", 0), "&hellip;");
        // test_truncate_string_multibyte_ascii
        assert_eq!(truncate_string("hello", 4), "hell&hellip;");
        // test_truncate_string_multibyte_unicode
        assert_eq!(truncate_string("café", 4), "café");
        // test_truncate_string_multibyte_unicode_boundary
        assert_eq!(truncate_string("café", 2), "ca&hellip;");
        // test_truncate_string_emoji
        assert_eq!(truncate_string("👋🌍", 2), "👋🌍");
        let r = truncate_string("👋🌍", 1);
        assert!(r.contains("👋") && r.contains("&hellip;"), "got: {r}");
        // test_truncate_string_cjk
        let r = truncate_string("你好世界", 3);
        assert!(r.contains("你") && r.contains("&hellip;"), "got: {r}");
    }

    #[test]
    fn color_of_matches_php() {
        // _color_of("") → sum 0 → colormap[0] = #1cd7d7
        assert_eq!(color_of(""), (0x1c, 0xd7, 0xd7));
        // verify against the oracle-computed value for a known title
        assert_eq!(color_unpack("#cc0000"), (204, 0, 0));
    }

    #[test]
    fn feed_to_label_id_roundtrip() {
        assert_eq!(feed_to_label_id(-1026), 1);
        assert_eq!(feed_to_label_id(-1033), 8);
    }

    #[test]
    fn basename_url() {
        assert_eq!(basename("https://x.com/a/b.jpg"), "b.jpg");
        assert_eq!(basename(""), "");
    }
}
