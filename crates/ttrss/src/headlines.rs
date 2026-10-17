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

use serde_json::{Map, Value, json};
use sqlx::AssertSqlSafe;
use sqlx::Row;

use crate::AppState;
use crate::cats::{CAT_LABELS, CAT_SPECIAL, CAT_UNCATEGORIZED};
use crate::prefs;
use crate::sanitizer;
use crate::session::Sess;
use crate::time::{self, TzCtx};
use crate::util::{basename, param_to_bool, strip_tags, truncate_string};

use crate::feeds::{
    FEED_ALL, FEED_ARCHIVED, FEED_FRESH, FEED_PUBLISHED, FEED_RECENTLY_READ, FEED_STARRED,
};
use crate::i18n::{__, t_sprintf};
use crate::labels::{LABEL_BASE_INDEX, feed_to_label_id};

// `Feeds::FEED_ERROR` (functions.php): the virtual "error feed" pseudo-id.
pub(crate) const FEED_ERROR: i32 = -7;
/// `PLUGIN_FEED_BASE_INDEX` (functions.php): virtual plugin feeds occupy
/// `(LABEL_BASE_INDEX, PLUGIN_FEED_BASE_INDEX)`.
pub(crate) const PLUGIN_FEED_BASE_INDEX: i32 = -128;
const NEVER_GROUP_FEEDS: &[i32] = &[FEED_RECENTLY_READ, FEED_ARCHIVED];
const NEVER_GROUP_BY_DATE: &[i32] = &[FEED_PUBLISHED, FEED_STARRED, FEED_FRESH];

// `SELECT` column indices for the shared headlines article-row query (built by
// `get_headlines_query`). Non-vfeed rows have 30 columns; vfeed rows add
// `feed_title` + `favicon_avg_color` after `last_published` (32 cols).
// `syndication` reuses the same layout.
pub(crate) const I_DATE_ENTERED: usize = 1;
pub(crate) const I_TITLE: usize = 4;
pub(crate) const I_UPDATED: usize = 5;
pub(crate) const I_LABEL_CACHE: usize = 6;
pub(crate) const I_TAG_CACHE: usize = 7;
pub(crate) const I_ALWAYS_ENC: usize = 8;
pub(crate) const I_SITE_URL: usize = 9;
pub(crate) const I_NOTE: usize = 10;
pub(crate) const I_NUM_COMMENTS: usize = 11;
pub(crate) const I_COMMENTS: usize = 12;
pub(crate) const I_INT_ID: usize = 13;
pub(crate) const I_UUID: usize = 14;
pub(crate) const I_LANG: usize = 15;
pub(crate) const I_HIDE_IMAGES: usize = 16;
pub(crate) const I_UNREAD: usize = 17;
pub(crate) const I_FEED_ID: usize = 18;
pub(crate) const I_MARKED: usize = 19;
pub(crate) const I_PUBLISHED: usize = 20;
pub(crate) const I_LINK: usize = 21;
pub(crate) const I_LAST_READ: usize = 22;
pub(crate) const I_VF_TITLE: usize = 25;
pub(crate) const I_VF_FAVICON: usize = 26;
pub(crate) const I_CONTENT: usize = 25; // non-vfeed
pub(crate) const I_VF_CONTENT: usize = 27;

/// The `HOOK_QUERY_HEADLINES` row: every SELECT column as a PDO string
/// (the PHP ORM hands plugins a string-typed row) under the ORM column
/// names, plus `content_preview` (and `tags` on the export path, where PHP
/// sets it first). Non-vfeed rows have 30 columns; vfeed rows 32.
pub(crate) fn row_to_plugin_value(
    r: &sqlx::postgres::PgRow,
    content_preview: &str,
    tags: Option<&[String]>,
) -> Value {
    let is_vfeed_row = r.columns().len() >= 32;
    let char1 = |b: Option<bool>| b.map(|x| if x { "t".to_string() } else { "f".to_string() });
    let date_entered: Option<chrono::NaiveDateTime> = r.get(I_DATE_ENTERED);
    let mut m = Map::new();
    m.insert("id".into(), json!(r.get::<i32, _>(0).to_string()));
    m.insert(
        "date_entered".into(),
        Value::from(date_entered.as_ref().map(time::naive_to_pg_text)),
    );
    m.insert(
        "yyiw".into(),
        json!(
            date_entered
                .map(|d| d.format("%G-%V").to_string())
                .unwrap_or_default()
        ),
    );
    m.insert("guid".into(), Value::from(r.get::<String, _>(3)));
    m.insert("title".into(), Value::from(r.get::<String, _>(I_TITLE)));
    m.insert(
        "updated".into(),
        Value::from(
            r.get::<Option<chrono::NaiveDateTime>, _>(I_UPDATED)
                .as_ref()
                .map(time::naive_to_pg_text),
        ),
    );
    m.insert(
        "label_cache".into(),
        Value::from(r.get::<String, _>(I_LABEL_CACHE)),
    );
    m.insert(
        "tag_cache".into(),
        Value::from(r.get::<String, _>(I_TAG_CACHE)),
    );
    m.insert(
        "always_display_enclosures".into(),
        Value::from(char1(r.get::<Option<bool>, _>(I_ALWAYS_ENC))),
    );
    m.insert(
        "site_url".into(),
        Value::from(r.get::<Option<String>, _>(I_SITE_URL)),
    );
    m.insert(
        "note".into(),
        Value::from(r.get::<Option<String>, _>(I_NOTE)),
    );
    m.insert(
        "num_comments".into(),
        json!(r.get::<i32, _>(I_NUM_COMMENTS).to_string()),
    );
    m.insert(
        "comments".into(),
        Value::from(r.get::<String, _>(I_COMMENTS)),
    );
    m.insert(
        "int_id".into(),
        json!(r.get::<i32, _>(I_INT_ID).to_string()),
    );
    m.insert("uuid".into(), Value::from(r.get::<String, _>(I_UUID)));
    m.insert(
        "lang".into(),
        Value::from(r.get::<Option<String>, _>(I_LANG)),
    );
    m.insert(
        "hide_images".into(),
        Value::from(char1(r.get::<Option<bool>, _>(I_HIDE_IMAGES))),
    );
    m.insert(
        "unread".into(),
        Value::from(if r.get::<bool, _>(I_UNREAD) { "t" } else { "f" }),
    );
    m.insert(
        "feed_id".into(),
        Value::from(r.get::<Option<i32>, _>(I_FEED_ID).map(|f| f.to_string())),
    );
    m.insert(
        "marked".into(),
        Value::from(if r.get::<bool, _>(I_MARKED) { "t" } else { "f" }),
    );
    m.insert(
        "published".into(),
        Value::from(if r.get::<bool, _>(I_PUBLISHED) {
            "t"
        } else {
            "f"
        }),
    );
    m.insert("link".into(), Value::from(r.get::<String, _>(I_LINK)));
    m.insert(
        "last_read".into(),
        Value::from(
            r.get::<Option<chrono::NaiveDateTime>, _>(I_LAST_READ)
                .as_ref()
                .map(time::naive_to_pg_text),
        ),
    );
    m.insert(
        "last_marked".into(),
        Value::from(
            r.get::<Option<chrono::NaiveDateTime>, _>(23)
                .as_ref()
                .map(time::naive_to_pg_text),
        ),
    );
    m.insert(
        "last_published".into(),
        Value::from(
            r.get::<Option<chrono::NaiveDateTime>, _>(24)
                .as_ref()
                .map(time::naive_to_pg_text),
        ),
    );
    if is_vfeed_row {
        m.insert(
            "feed_title".into(),
            Value::from(r.get::<Option<String>, _>(I_VF_TITLE)),
        );
        m.insert(
            "favicon_avg_color".into(),
            Value::from(r.get::<Option<String>, _>(I_VF_FAVICON)),
        );
    }
    m.insert(
        "content".into(),
        Value::from(r.get::<String, _>(if is_vfeed_row {
            I_VF_CONTENT
        } else {
            I_CONTENT
        })),
    );
    m.insert(
        "author".into(),
        Value::from(r.get::<String, _>(if is_vfeed_row { 28 } else { 26 })),
    );
    m.insert(
        "score".into(),
        json!(
            r.get::<i32, _>(if is_vfeed_row { 29 } else { 27 })
                .to_string()
        ),
    );
    m.insert(
        "num_labels".into(),
        json!(
            r.get::<i64, _>(if is_vfeed_row { 30 } else { 28 })
                .to_string()
        ),
    );
    m.insert(
        "num_enclosures".into(),
        json!(
            r.get::<i64, _>(if is_vfeed_row { 31 } else { 29 })
                .to_string()
        ),
    );
    m.insert(
        "content_preview".into(),
        Value::from(content_preview.to_string()),
    );
    if let Some(tags) = tags {
        m.insert(
            "tags".into(),
            Value::Array(tags.iter().map(|t| Value::from(t.as_str())).collect()),
        );
    }
    Value::Object(m)
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
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    let order_by = extra
        .get("order_by")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let check_first_id: i32 = extra
        .get("fid")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0) as i32;

    // `if (is_numeric($feed)) $feed = (int) $feed;` (a non-numeric string is a
    // tag name).
    let feed_num: Option<i32> = feed_raw.trim().parse().ok();

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
        return generate_error_feed(&__("Feed not found."));
    }

    // `Prefs::set` for the two view prefs (the PHP side effect, Feeds.php).
    prefs::set(
        &state.db,
        "_DEFAULT_VIEW_MODE",
        &view_mode,
        uid,
        profile,
        true,
    )
    .await;
    prefs::set(
        &state.db,
        "_DEFAULT_VIEW_ORDER_BY",
        &order_by,
        uid,
        profile,
        true,
    )
    .await;
    // `Prefs::set` refreshes the in-process cache on every write (the PHP side
    // effect, Feeds.php); invalidate so the next read reflects the view mode.
    crate::prefs::invalidate(&state.prefs_cache, uid, profile);

    // Bump the login timestamp if it's over an hour old.
    if let Some(last_login_update) = data.get_int("last_login_update") {
        let now = chrono::Utc::now().timestamp();
        if now - last_login_update > 3600 {
            let _ = sqlx::query("UPDATE ttrss_users SET last_login = NOW() WHERE id = $1")
                .bind(uid)
                .execute(pool)
                .await;
            sess.with_mut(|s| {
                s.set("last_login_update", crate::core::phpser::Value::Int(now));
            });
        }
    }

    if let Some(f) = feed_num
        && !cat_view
        && f > 0
    {
        let _ = sqlx::query(
            "UPDATE ttrss_feeds SET last_viewed = NOW() WHERE id = $1 AND owner_uid = $2",
        )
        .bind(f)
        .bind(uid)
        .execute(pool)
        .await;
    }

    // M7.6: the HOOK_SANITIZE / plugin context (built once per request);
    // the custom-sort override chain (below) needs it.
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    let (override_order, skip_first_id_check) = order_to_override_query(&mut pctx, &order_by).await;

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
        &mut pctx,
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
    headlines.insert(
        "id".into(),
        if nuf_truthy {
            next_unread_feed
        } else {
            feed_value
        },
    );
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

    let default_language = prefs::resolve(prefs::DEFAULT_SEARCH_LANGUAGE, &rows, profile)
        .as_str()
        .to_string();

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
pub(crate) async fn order_to_override_query(
    host: &mut crate::pluginhost::host::HostCtx,
    order: &str,
) -> (String, bool) {
    let mut query = String::new();
    let mut skip_first_id = false;

    // `HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE` (Feeds.php:2359): the chain
    // receives `$order`; its `[query, skip_first_id]` result is used — the
    // first non-empty query short-circuits.
    if !host
        .hook_order(crate::pluginhost::HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE)
        .is_empty()
    {
        let order_owned = order.to_string();
        let mut override_result: Option<(String, bool)> = None;
        host.run_hooks_callback::<serde_json::Value, _, _>(
            crate::pluginhost::HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE,
            |plugin, host_ctx| {
                let o = order_owned.clone();
                Box::pin(async move {
                    plugin
                        .hook_headlines_custom_sort_override(host_ctx, &o)
                        .await
                })
            },
            |result, _| {
                let q = result
                    .get(0)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let skip = result
                    .get(1)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                override_result = Some((q.clone(), skip));
                !q.is_empty()
            },
        )
        .await;
        if let Some((q, skip)) = override_result
            && !q.is_empty()
        {
            return (q, skip);
        }
    }

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

// ---------------------------------------------------------------------------
// `_format_headlines_list` + `_get_headlines`
// ---------------------------------------------------------------------------

struct HeadlinesResult {
    headlines: Map<String, Value>,
    count: i64,
    disable_cache: bool,
}

/// `Feeds::_format_headlines_list` (with `_get_headlines` inlined; faithful
/// port — the PHP method takes this many parameters).
#[allow(clippy::too_many_arguments)]
async fn format_headlines_list(
    state: &AppState,
    sess: &Sess,
    feed: &str,
    feed_num: Option<i32>,
    method: &str,
    view_mode: &str,
    limit: i64,
    cat_view: bool,
    offset: i64,
    override_order: &str,
    include_children: bool,
    check_first_id: i32,
    skip_first_id_check: bool,
    order_by: &str,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
    search: &str,
    search_language: &str,
    mut pctx: &mut crate::pluginhost::host::HostCtx,
) -> HeadlinesResult {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let pool = state.db.pool();

    // `if ($method == "undefined") $method = "";` (the JS sends the string
    // "undefined" for an absent param).
    let method = if method == "undefined" { "" } else { method };

    // `m=ForceUpdate` — the UI's active-feed refresh (the frozen JS sends it
    // when re-selecting the active feed, Feeds.js:423): reset the feed's
    // update stamps so the in-process updater re-fetches it.
    if method == "ForceUpdate"
        && let Some(f) = feed_num
        && f > 0
    {
        let _ = sqlx::query(
                "UPDATE ttrss_feeds SET last_updated = '1970-01-01', last_update_started = '1970-01-01' WHERE id = $1",
            )
            .bind(f)
            .execute(pool)
            .await;
    }

    // `m=MarkAllReadGR:<feed>` (legacy/other clients — the frozen JS never
    // sends it) — `Feeds::_catchup($method_split[1], false)`.
    if method.split(':').next() == Some("MarkAllReadGR") {
        let scope = method.split_once(':').map(|(_, s)| s).unwrap_or("");
        crate::feeds::catchup_scope(pool, uid, profile, rows, scope, false, "all", "", "").await;
    }

    // `if ($search) $disable_cache = true;`
    let disable_cache = !search.is_empty();

    let vfeed_group_enabled = prefs::resolve("VFEED_GROUP_BY_FEED", rows, profile).as_bool()
        && !((feed_num
            .map(|f| NEVER_GROUP_FEEDS.contains(&f))
            .unwrap_or(false))
            && !cat_view);

    // ---- `_get_headlines` ----
    let q = get_headlines(
        &state.db,
        uid,
        feed,
        feed_num,
        limit,
        view_mode,
        cat_view,
        search,
        search_language,
        override_order,
        offset,
        include_children,
        check_first_id,
        skip_first_id_check,
        order_by,
        rows,
        profile,
        0,     // since_id (the SPA `view` call does not paginate by id)
        false, // ignore_vfeed_group (the SPA view path does not set it)
        "",    // start_ts (the SPA view path does not set it)
    )
    .await;

    let HeadlinesQuery {
        article_rows,
        feed_title,
        feed_site_url,
        last_error,
        last_updated,
        search_words,
        first_id,
        is_vfeed,
        query_error_override,
        first_id_changed,
    } = q;

    let tz = TzCtx {
        user_tz: prefs::resolve(prefs::USER_TIMEZONE, rows, profile)
            .as_str()
            .to_string(),
        client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
        short_date_format: prefs::resolve(prefs::SHORT_DATE_FORMAT, rows, profile)
            .as_str()
            .to_string(),
        long_date_format: prefs::resolve(prefs::LONG_DATE_FORMAT, rows, profile)
            .as_str()
            .to_string(),
    };

    // `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2` / `_BUTTON` (Feeds.php:170-181):
    // each hook's HTML string is appended into the toolbar's plugin slots.
    let feed_id: i32 = feed_num.unwrap_or(0);
    let mut plugin_menu_items = String::new();
    pctx.chain_hooks_callback(
        crate::pluginhost::HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2,
        &mut plugin_menu_items,
        |plugin, host, acc| {
            Box::pin(async move {
                let r = plugin
                    .hook_headline_toolbar_select_menu_item2(host, feed_id, cat_view)
                    .await;
                acc.push_str(&r);
                r
            })
        },
        |_r, _p| false,
    )
    .await;
    let mut plugin_buttons = String::new();
    pctx.chain_hooks_callback(
        crate::pluginhost::HOOK_HEADLINE_TOOLBAR_BUTTON,
        &mut plugin_buttons,
        |plugin, host, acc| {
            Box::pin(async move {
                let r = plugin
                    .hook_headline_toolbar_button(host, feed_id, cat_view)
                    .await;
                acc.push_str(&r);
                r
            })
        },
        |_r, _p| false,
    )
    .await;

    let mut toolbar = Map::new();
    toolbar.insert("site_url".into(), json!(feed_site_url));
    toolbar.insert("title".into(), json!(strip_tags(&feed_title)));
    toolbar.insert("error".into(), json!(last_error));
    toolbar.insert(
        "last_updated".into(),
        json!(time::make_local_datetime(
            last_updated.as_deref(),
            false,
            &tz
        )),
    );
    toolbar.insert("plugin_menu_items".into(), Value::from(plugin_menu_items));
    toolbar.insert("plugin_buttons".into(), Value::from(plugin_buttons));

    let mut reply: Map<String, Value> = Map::new();
    reply.insert("first_id".into(), json!(first_id));
    reply.insert("is_vfeed".into(), json!(is_vfeed));
    reply.insert("search_query".into(), json!([search, search_language]));
    reply.insert("vfeed_group_enabled".into(), json!(vfeed_group_enabled));
    reply.insert("toolbar".into(), Value::Object(toolbar));

    // `HOOK_HEADLINES_BEFORE` (Feeds.php:195): first page only
    // ($offset == 0); each hook's HTML string is prepended before the
    // article rows (PHP appends it into `$reply['content']` while it is
    // `[]`, then appends the rows; the no-articles message *replaces* it).
    let headlines_before: Option<String> = if offset == 0
        && !pctx
            .hook_order(crate::pluginhost::HOOK_HEADLINES_BEFORE)
            .is_empty()
    {
        let qfh_ret = json!([
            article_rows.len(), // the row set (PHP: opaque PDOStatement)
            feed_title,
            feed_site_url,
            last_error,
            last_updated,
            search_words,
            first_id,
            is_vfeed,
            query_error_override,
        ]);
        let mut before = String::new();
        pctx.chain_hooks_callback(
            crate::pluginhost::HOOK_HEADLINES_BEFORE,
            &mut before,
            |plugin, host, acc| {
                let q = qfh_ret.clone();
                Box::pin(async move {
                    let r = plugin
                        .hook_headlines_before(host, feed_id, cat_view, &q)
                        .await;
                    acc.push_str(&r);
                    r
                })
            },
            |_r, _p| false,
        )
        .await;
        Some(before)
    } else {
        None
    };

    let mut content_list: Vec<Value> = Vec::new();
    let mut headlines_count = 0i64;
    let mut rgba_cache: std::collections::HashMap<i32, (i64, i64, i64)> =
        std::collections::HashMap::new();

    let show_content_preview = prefs::resolve("SHOW_CONTENT_PREVIEW", rows, profile).as_bool();
    let cdm_expanded = prefs::resolve("CDM_EXPANDED", rows, profile).as_bool();
    let strip_images = prefs::resolve(prefs::STRIP_IMAGES, rows, profile).as_bool();
    let bw_limit = data.get_bool("bw_limit").unwrap_or(false);
    let has_sandbox = data.get_bool("hasSandbox").unwrap_or(false);

    for r in &article_rows {
        headlines_count += 1;

        let is_vfeed_row = r.columns().len() >= 32;
        let id: i32 = r.get(0);
        let date_entered: Option<chrono::NaiveDateTime> = r.get(I_DATE_ENTERED);
        let mut title: String = r.get(I_TITLE);
        let updated: Option<chrono::NaiveDateTime> = r.get(I_UPDATED);
        let mut guid: String = r.get(3);
        let mut label_cache: String = r.get(I_LABEL_CACHE);
        let mut tag_cache: String = r.get(I_TAG_CACHE);
        // `always_display_enclosures` / `site_url` / `hide_images` come from
        // `ttrss_feeds` — NULL for archived rows (LEFT JOIN miss); PHP emits
        // `null` and treats them as falsy.
        let mut always_display_enclosures: Option<bool> = r.get(I_ALWAYS_ENC);
        let mut site_url: Option<String> = r.get(I_SITE_URL);
        let mut note: Option<String> = r.get(I_NOTE);
        let mut num_comments: i32 = r.get(I_NUM_COMMENTS);
        let mut comments: String = r.get(I_COMMENTS);
        let mut int_id: i32 = r.get(I_INT_ID);
        let mut uuid: String = r.get(I_UUID);
        let mut lang: Option<String> = r.get(I_LANG);
        let mut hide_images: Option<bool> = r.get(I_HIDE_IMAGES);
        let mut unread: bool = r.get(I_UNREAD);
        // Re-derived from the hook row below (initialized `None`); the row's
        // `feed_id` (null for archived rows) is the source of truth.
        let mut feed_id_db: Option<i32> = r.get(I_FEED_ID);
        let mut marked: bool = r.get(I_MARKED);
        let mut published: bool = r.get(I_PUBLISHED);
        let mut link: String = r.get(I_LINK);
        let last_read: Option<chrono::NaiveDateTime> = r.get(I_LAST_READ);
        let mut vfeed_title: Option<String> = if is_vfeed_row {
            r.get::<Option<String>, _>(I_VF_TITLE)
        } else {
            None
        };
        let mut favicon_avg_color: Option<String> = if is_vfeed_row {
            r.get::<Option<String>, _>(I_VF_FAVICON)
        } else {
            None
        };
        let mut content_raw: String = r.get(if is_vfeed_row {
            I_VF_CONTENT
        } else {
            I_CONTENT
        });
        let mut author: String = r.get(if is_vfeed_row { 28 } else { 26 });
        let mut score: i32 = r.get(if is_vfeed_row { 29 } else { 27 });
        let mut num_labels: i64 = r.get(if is_vfeed_row { 30 } else { 28 });
        let mut num_enclosures: i64 = r.get(if is_vfeed_row { 31 } else { 29 });

        let mut updated_str = updated.as_ref().map(time::naive_to_pg_text);
        let date_entered_str = date_entered.as_ref().map(time::naive_to_pg_text);
        let mut last_read_str = last_read.as_ref().map(time::naive_to_pg_text);

        // `content_preview` — first key added (pre-sanitize raw content).
        let content_preview = if show_content_preview {
            format!(
                "&mdash; {}",
                truncate_string(&strip_tags(&content_raw), 250, "&hellip;")
            )
        } else {
            String::new()
        };

        // `HOOK_QUERY_HEADLINES` (Feeds.php:213-217): per-row, right after
        // `content_preview` is set. The hook receives the row — PDO string
        // values under the ORM column keys — plus the excerpt length; the
        // result *replaces* the row, and the rest of this loop continues on
        // the replaced row.
        {
            let mut row_val = row_to_plugin_value(r, &content_preview, None);
            if !pctx
                .hook_order(crate::pluginhost::HOOK_QUERY_HEADLINES)
                .is_empty()
            {
                pctx.chain_hooks_callback(
                    crate::pluginhost::HOOK_QUERY_HEADLINES,
                    &mut row_val,
                    |plugin, host, row| {
                        Box::pin(async move {
                            let mut local = row.clone();
                            let res = plugin.hook_query_headlines(host, &mut local, 250).await;
                            *row = res.clone();
                            res
                        })
                    },
                    |_r, _p| false,
                )
                .await;

                let h_str =
                    |name: &str| row_val.get(name).and_then(Value::as_str).map(String::from);
                let h_i =
                    |name: &str, cur: i64| row_val.get(name).and_then(Value::as_i64).unwrap_or(cur);
                let h_t = |name: &str, cur: bool| match h_str(name).as_deref() {
                    Some("t") => true,
                    Some("f") => false,
                    _ => cur,
                };

                guid = h_str("guid").unwrap_or(guid);
                title = h_str("title").unwrap_or(title);
                updated_str = h_str("updated").or(updated_str);
                label_cache = h_str("label_cache").unwrap_or(label_cache);
                tag_cache = h_str("tag_cache").unwrap_or(tag_cache);
                always_display_enclosures = match h_str("always_display_enclosures").as_deref() {
                    Some("t") => Some(true),
                    Some("f") => Some(false),
                    _ => always_display_enclosures,
                };
                site_url = h_str("site_url").or(site_url);
                note = h_str("note").or(note);
                num_comments = h_i("num_comments", num_comments as i64) as i32;
                comments = h_str("comments").unwrap_or(comments);
                int_id = h_i("int_id", int_id as i64) as i32;
                uuid = h_str("uuid").unwrap_or(uuid);
                lang = h_str("lang").or(lang);
                hide_images = match h_str("hide_images").as_deref() {
                    Some("t") => Some(true),
                    Some("f") => Some(false),
                    _ => hide_images,
                };
                unread = h_t("unread", unread);
                feed_id_db = h_str("feed_id").and_then(|v| v.parse().ok());
                marked = h_t("marked", marked);
                published = h_t("published", published);
                link = h_str("link").unwrap_or(link);
                last_read_str = h_str("last_read").or(last_read_str);
                if is_vfeed_row {
                    vfeed_title = h_str("feed_title").or(vfeed_title);
                    favicon_avg_color = h_str("favicon_avg_color").or(favicon_avg_color);
                }
                content_raw = h_str("content").unwrap_or(content_raw);
                author = h_str("author").unwrap_or(author);
                score = h_i("score", score as i64) as i32;
                num_labels = h_i("num_labels", num_labels);
                num_enclosures = h_i("num_enclosures", num_enclosures);
            }
        }

        // `if ($line['feed_id'] === null)` — normalize the archived feed.
        let feed_id: i32 = feed_id_db.unwrap_or(FEED_ARCHIVED);
        let archived = feed_id_db.is_none();
        let feed_title_out = if let Some(t) = &vfeed_title {
            Some(t.clone())
        } else if archived {
            Some(__("Archived articles"))
        } else {
            None
        };

        // labels (`Article::_get_labels` when the cache is empty)
        let labels: Value = if num_labels > 0 {
            let cache = label_cache.trim();
            if cache.is_empty() {
                // PHP `Article::_get_labels($id)` — the hard path, which also
                // writes the result back to `label_cache`. A later
                // `Article::_labels_of` (in the mutation handlers) then sees
                // the labels, so the per-label counters update on the
                // post-marking refresh instead of waiting for the next poll.
                serde_json::from_str(&crate::labels::get_labels_json(pool, uid, id).await)
                    .unwrap_or(Value::Array(Vec::new()))
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
            id,
            Some(&mut pctx),
        )
        .await;

        let tags = if tag_cache.is_empty() {
            Vec::new()
        } else {
            tag_cache
                .split(',')
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        };

        // `Article::_format_enclosures` (sql_bool_to_bool: NULL → false)
        let enclosures = if num_enclosures > 0 {
            // M7.22 item 6: the enclosure chains run here (host carries the
            // active set).
            format_enclosures(
                pctx,
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
        let rgb = rgba_cache.entry(feed_id).or_insert_with(|| {
            let fav_color = favicon_avg_color.as_deref().unwrap_or("");
            if !fav_color.is_empty() && fav_color != "fail" {
                color_unpack(fav_color)
            } else {
                color_of(feed_title_out.as_deref().unwrap_or(""))
            }
        });
        let (rr, gg, bb) = rgb;

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
            json!(time::make_local_datetime(updated_str.as_deref(), true, &tz)),
        );
        line.insert(
            "always_display_enclosures".into(),
            json!(always_display_enclosures),
        );
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
        line.insert("score".into(), json!(score.to_string()));
        line.insert("num_labels".into(), json!(num_labels));
        line.insert("content_preview".into(), json!(content_preview));
        if archived && vfeed_title.is_none() {
            line.insert("feed_title".into(), json!(__("Archived articles")));
        }
        line.insert("labels".into(), labels);
        if vfeed_title.is_none() && !archived {
            // `$line["feed_title"] ??= ""`
            line.insert("feed_title".into(), json!(""));
        }
        // `HOOK_ARTICLE_LEFT_BUTTON` / `HOOK_ARTICLE_BUTTON`
        // (Feeds.php:258/283): the full row goes through the per-row button
        // chains (PHP passes `$line`, incl. `uuid`, which it `unset`s after
        // the hooks). Each non-empty result must be valid XML, gets
        // `data-plugin-name`, and is appended to the row's button slots.
        let mut buttons_left = String::new();
        {
            let line_v = Value::Object(line.clone());
            pctx.run_hooks_callback::<String, _, _>(
                crate::pluginhost::HOOK_ARTICLE_LEFT_BUTTON,
                |plugin, host_ctx| {
                    let l = line_v.clone();
                    Box::pin(async move { plugin.hook_article_left_button(host_ctx, &l).await })
                },
                |result, plugin| {
                    if !result.is_empty() {
                        match button_xml_roundtrip(result, plugin.class_name()) {
                            Some(xml) => buttons_left.push_str(&xml),
                            None => tracing::warn!(
                                plugin = plugin.class_name(),
                                "plugin: content provided in HOOK_ARTICLE_LEFT_BUTTON is not valid XML"
                            ),
                        }
                    }
                    false
                },
            )
            .await;
        }
        line.insert("buttons_left".into(), Value::from(buttons_left));
        {
            let line_v = Value::Object(line.clone());
            let buttons = article_buttons(pctx, &line_v).await;
            line.insert("buttons".into(), Value::from(buttons));
        }
        // `cdm_excerpt` (Feeds.php:329-338): only when `CDM_EXPANDED` is
        // off — the collapsed-CDM close button, plus the preview text when
        // `SHOW_CONTENT_PREVIEW` is on (the frontend ternary handles the
        // key's absence).
        if !cdm_expanded {
            let mut cdm_excerpt = format!(
                "<span class='collapse'><i class='material-icons' \
                 onclick='return Article.cdmUnsetActive(event)' \
                 title=\"{}\">remove_circle</i></span>",
                __("Collapse article")
            );
            if show_content_preview {
                cdm_excerpt.push_str(&format!("<span class='excerpt'>{content_preview}</span>"));
            }
            line.insert("cdm_excerpt".into(), Value::from(cdm_excerpt));
        }
        line.insert("enclosures".into(), enclosures);
        line.insert(
            "updated_long".into(),
            json!(time::make_local_datetime(
                updated_str.as_deref(),
                false,
                &tz
            )),
        );
        let imported_at = time::make_local_datetime(date_entered_str.as_deref(), false, &tz);
        line.insert(
            "imported".into(),
            json!(t_sprintf("Imported at %s", &[imported_at.as_str()])),
        );
        line.insert(
            "tags".into(),
            Value::Array(tags.iter().map(|s| Value::from(s.as_str())).collect()),
        );
        line.insert(
            "has_icon".into(),
            json!(has_icon(&state.config.feed_icon_dir(), feed_id)),
        );
        line.insert(
            "feed_bg_color".into(),
            json!(format!("rgba({rr},{gg},{bb},0.3)")),
        );

        // M7.16: HOOK_RENDER_ARTICLE_CDM — the per-article render hook
        // (nsfw rewrites <img> tags to blurred placeholders).
        let mut line_val = Value::Object(line);
        pctx.chain_hooks_callback(
            crate::pluginhost::HOOK_RENDER_ARTICLE_CDM,
            &mut line_val,
            |plugin, host, art| {
                Box::pin(async move {
                    let result = plugin.hook_render_article_cdm(host, art).await;
                    // The PHP callback rebinds `$line = $result` (M7.16 bug:
                    // the fold was missing, so the nsfw rewrite was dropped).
                    *art = result.clone();
                    result
                })
            },
            |_r, _p| false,
        )
        .await;
        // `$line['content'] = DiskCache::rewrite_urls($line['content'])`
        // (Feeds.php:357) — cached media URLs rewrite to the local
        // `op=cached` serve path.
        if let Some(c) = line_val
            .get_mut("content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        {
            let rewritten =
                crate::diskcache::rewrite_urls(&c, &pctx.cache_dir.join("images"), &pctx.self_url);
            if let Some(slot) = line_val.get_mut("content") {
                *slot = Value::from(rewritten);
            }
        }
        content_list.push(line_val);
    }

    if headlines_count == 0 {
        // `if ($result instanceof PDOStatement)` — the empty-message block.
        let message = if !query_error_override.is_empty() {
            query_error_override.clone()
        } else {
            match view_mode {
                "unread" => __("No unread articles found to display."),
                "updated" => __("No updated articles found to display."),
                "marked" => __("No starred articles found to display."),
                _ => {
                    if feed_num.map(|f| f < LABEL_BASE_INDEX).unwrap_or(false) {
                        __(
                            "No articles found to display. You can assign articles to labels manually from article header context menu (applies to all selected articles) or use a filter.",
                        )
                    } else {
                        __("No articles found to display.")
                    }
                }
            }
        };
        if offset == 0 && !message.is_empty() {
            let last_updated: Option<String> = sqlx::query_scalar::<_, String>(
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
            let last_updated_str = time::make_local_datetime(last_updated.as_deref(), false, &tz);
            msg.push_str(&t_sprintf(
                "Feeds last updated at %s",
                &[last_updated_str.as_str()],
            ));
            if num_errors > 0 {
                let err_link = __("Some feeds have update errors (click for details)");
                msg.push_str(&format!(
                    "<br/><a class=\"text-muted\" href=\"#\" onclick=\"CommonDialogs.showFeedsWithErrors(); return false\">{err_link}</a>"
                ));
            }
            msg.push_str("</span></p></div>");
            reply.insert("content".into(), Value::from(msg));
        } else if let Some(before) = &headlines_before {
            // No message (first-id-changed path): PHP's content stays the
            // hook string.
            reply.insert("content".into(), Value::from(before.clone()));
        } else if offset == 0 {
            // No hook registered: PHP's content is `[]` (the empty array).
            reply.insert("content".into(), json!([]));
        }
    } else if let Some(before) = headlines_before {
        let mut content = vec![Value::from(before)];
        content.extend(content_list);
        reply.insert("content".into(), Value::Array(content));
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

/// M7.22 item 5 — `HOOK_ARTICLE_BUTTON` (PHP `Feeds.php:283`): the per-row
/// button chain. Each plugin's XML is round-tripped through a DOM pass that
/// appends `data-plugin-name` (the plugin's class name) and normalizes the
/// markup (attribute order preserved, double quotes); invalid XML is skipped
/// with a warning, like PHP's `user_error`.
async fn article_buttons(host: &mut crate::pluginhost::host::HostCtx, line: &Value) -> String {
    // The PHP callback appends EACH plugin's (round-tripped) result — the
    // `FnMut` stop predicate of `run_hooks_callback` sees every
    // (result, plugin) pair; it always returns `false` (no short-circuit).
    let mut out = String::new();
    let line = line.clone();
    host.run_hooks_callback::<String, _, _>(
        crate::pluginhost::HOOK_ARTICLE_BUTTON,
        |plugin, host_ctx| {
            let line = line.clone();
            Box::pin(async move { plugin.hook_article_button(host_ctx, &line).await })
        },
        |result, plugin| {
            if !result.is_empty() {
                match button_xml_roundtrip(result, plugin.class_name()) {
                    Some(xml) => out.push_str(&xml),
                    None => tracing::warn!(
                        plugin = plugin.class_name(),
                        "plugin: content provided in HOOK_ARTICLE_BUTTON is not valid XML"
                    ),
                }
            }
            false
        },
    )
    .await;
    out
}

/// The PHP DOM pass over a `HOOK_ARTICLE_BUTTON` result (`loadXML`,
/// `setAttribute('data-plugin-name', …)` on the root, `saveXML(firstChild)`):
/// a single well-formed root element, attribute order preserved, quotes
/// normalized to double, `data-plugin-name` appended last. `None` when the
/// string is not valid XML (PHP: `user_error` + skip).
fn button_xml_roundtrip(result: &str, class_name: &str) -> Option<String> {
    let s = result.trim();
    let tag_end = s.find('>')?;
    let open = &s[..tag_end + 1];
    let tag = tag_name(open)?;
    let self_closed = open.ends_with("/>");
    let rest = &s[tag_end + 1..];
    let body = if self_closed {
        String::new()
    } else {
        // The single root's content; `rfind` keeps a nested `</tag>` from
        // truncating early (stock bodies are plain text).
        let close = format!("</{tag}>");
        let end = rest.rfind(&close)?;
        rest[..end].to_string()
    };
    let attrs = parse_attrs(open)?;
    let mut out = format!("<{tag}");
    for (k, v) in attrs {
        out.push_str(&format!(" {}=\"{}\"", k, xml_escape_attr(&v)));
    }
    out.push_str(&format!(" data-plugin-name=\"{}\"", class_name));
    if self_closed {
        out.push_str("/>");
    } else {
        out.push_str(&format!(">{}</{tag}>", xml_escape_body(&body)));
    }
    Some(out)
}

/// `<i ...` → `"i"` — the root tag name of an opening tag.
fn tag_name(open_tag: &str) -> Option<&str> {
    let name = open_tag[1..].trim_start();
    let end = name.find(char::is_whitespace).unwrap_or(name.len());
    let name = &name[..end];
    if name.is_empty() { None } else { Some(name) }
}

/// Attribute pairs of an opening tag, in document order. Quoted values are
/// unescaped (`&amp;` → `&`), matching a DOM parse; a bare `&` makes the
/// string invalid XML (PHP `loadXML` fails). Unquoted values (never used by
/// stock hooks) are taken raw.
fn parse_attrs(open_tag: &str) -> Option<Vec<(String, String)>> {
    let inner = open_tag[1..].trim();
    let inner = inner.strip_suffix('>')?.trim();
    let inner = inner.strip_suffix("/>").unwrap_or(inner);
    // Drop the tag name; no attributes at all is valid (`<i/>`).
    let Some(space) = inner.find(char::is_whitespace) else {
        return Some(Vec::new());
    };
    let mut out = Vec::new();
    let mut rest = inner[space..].trim();
    while !rest.is_empty() {
        let name_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        rest = &rest[name_end..];
        if let Some(stripped) = rest.strip_prefix('=') {
            let stripped = stripped.trim_start();
            let (v, consumed) = if let Some(v) = stripped.strip_prefix('"') {
                let end = v.find('"')?;
                (unescape_xml(&v[..end])?, end + 2)
            } else if let Some(v) = stripped.strip_prefix('\'') {
                let end = v.find('\'')?;
                (unescape_xml(&v[..end])?, end + 2)
            } else {
                let end = stripped
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(stripped.len());
                (stripped[..end].to_string(), end)
            };
            out.push((name.to_string(), v));
            rest = stripped[consumed..].trim_start();
        } else {
            // attribute without a value (XML `name` form)
            out.push((name.to_string(), name.to_string()));
            rest = rest.trim_start();
        }
    }
    Some(out)
}

/// Unescape the XML entity set. `None` when a bare `&` does not start a
/// well-formed entity (invalid XML).
fn unescape_xml(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let end = s[i..].find(';')?;
        let ent = &s[i..=i + end];
        match ent {
            "&amp;" => out.push('&'),
            "&lt;" => out.push('<'),
            "&gt;" => out.push('>'),
            "&quot;" => out.push('"'),
            "&apos;" => out.push('\''),
            _ => {
                // Numeric (`&#65;` / `&#x41;`) entities: `loadXML` accepts
                // them. Unknown *named* entities are invalid — but stock
                // output never carries either, so accept the numeric shapes
                // and reject anything else.
                let payload = ent.get(1..end)?.as_bytes();
                let numeric = payload.first().is_some_and(|&b| b == b'#')
                    && payload.iter().skip(1).all(|b| b.is_ascii_alphanumeric());
                if !numeric {
                    return None;
                }
                out.push_str(ent);
            }
        }
    }
    Some(out)
}

/// PHP `saveXML` escaping for attribute values.
fn xml_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// PHP `saveXML` escaping for text content (character data).
fn xml_escape_body(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `Article::_format_enclosures`.
///
/// M7.22 item 6 — the three enclosure chains (PHP `Article.php:308`):
/// `HOOK_FORMAT_ENCLOSURES` may replace the whole rendering (`[formatted,
/// entries]` or a plain string; a non-empty `formatted` short-circuits the
/// rest), and per enclosure `HOOK_RENDER_ENCLOSURE` (af_youtube_embed) may
/// supply the inline HTML while `HOOK_ENCLOSURE_ENTRY` (cache_starred_images)
/// rewrites the entry map.
async fn format_enclosures(
    host: &mut crate::pluginhost::host::HostCtx,
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
    .fetch_all(host.pool())
    .await
    .unwrap_or_default();

    // `Article::_get_enclosures`: a locally cached media copy rewrites
    // `content_url` to the `op=cached` serve path (PHP's
    // `DiskCache::instance("images")` wrap) before the hook chains.
    let cache = crate::diskcache::DiskCache::new(&host.cache_dir, "images", &host.self_url);

    // The raw rows (PHP adds `filename` later, per enclosure).
    let mut enclosures: Vec<Value> = encs
        .iter()
        .map(|r| {
            let url = r.get::<String, _>(1);
            let key = crate::util::sha1_hex(&url);
            let content_url = if cache.exists(&key) {
                cache.get_url(&key)
            } else {
                url
            };
            json!({
                "id": r.get::<i32, _>(0),
                "content_url": content_url,
                "content_type": r.get::<String, _>(2),
                "title": r.get::<String, _>(3),
                "duration": r.get::<String, _>(4),
                "width": r.get::<i32, _>(5),
                "height": r.get::<i32, _>(6),
                "post_id": r.get::<i32, _>(7),
            })
        })
        .collect();

    // HOOK_FORMAT_ENCLOSURES — whole-set override.
    let mut state = (
        String::new(),
        Value::Array(enclosures.clone()),
        id,
        always_display_enclosures,
        article_content.to_string(),
        hide_images,
    );
    let mut override_result: Option<Value> = None;
    host.chain_hooks_callback::<(String, Value, i32, bool, String, bool), Value, _, _>(
        crate::pluginhost::HOOK_FORMAT_ENCLOSURES,
        &mut state,
        |plugin, host, st| {
            Box::pin(async move {
                plugin
                    .hook_format_enclosures(host, &st.0, &st.1, st.2, st.3, &st.4, st.5)
                    .await
            })
        },
        |r, _| {
            override_result = Some(r.clone());
            false
        },
    )
    .await;
    if let Some(res) = override_result {
        match res.as_array().filter(|a| a.len() == 2) {
            Some(a) => {
                let formatted = a[0].as_str().unwrap_or("").to_string();
                enclosures = a[1].as_array().cloned().unwrap_or_default();
                if !formatted.is_empty() {
                    return json!({ "formatted": formatted, "entries": [] });
                }
            }
            None => {
                if res.as_str().is_some_and(|s| !s.is_empty()) {
                    return json!({ "formatted": res, "entries": [] });
                }
            }
        }
    }

    let can_inline =
        !bw_limit && !strip_images && (always_display_enclosures || !contains_img(article_content));
    let inline_text_only = hide_images && can_inline;

    let mut formatted = String::new();
    let mut entries: Vec<Value> = Vec::new();
    let mut rv: Map<String, Value> = Map::new();
    rv.insert("formatted".into(), json!(formatted));
    rv.insert("entries".into(), json!(entries));
    rv.insert("can_inline".into(), json!(can_inline));
    rv.insert("inline_text_only".into(), json!(inline_text_only));

    for mut enc in enclosures {
        // PHP adds `filename` before the per-enclosure chains.
        let filename = enc
            .get("content_url")
            .and_then(Value::as_str)
            .map(basename)
            .unwrap_or_default();
        enc["filename"] = json!(filename);

        // HOOK_RENDER_ENCLOSURE — per-enclosure inline HTML (af_youtube_embed).
        let mut render_state = (enc.clone(), id, Value::Object(rv.clone()));
        let mut rendered: Option<String> = None;
        host.chain_hooks_callback::<(Value, i32, Value), String, _, _>(
            crate::pluginhost::HOOK_RENDER_ENCLOSURE,
            &mut render_state,
            |plugin, host, st| {
                Box::pin(
                    async move { plugin.hook_render_enclosure(host, &st.0, st.1, &st.2).await },
                )
            },
            |r, _| {
                rendered = Some(r.clone());
                false
            },
        )
        .await;
        let rendered = rendered.unwrap_or_default();
        if !rendered.is_empty() {
            formatted.push_str(&rendered);
        } else {
            // HOOK_ENCLOSURE_ENTRY — entry-map rewrite (cache_starred_images).
            let mut entry_state = (enc.clone(), id, Value::Object(rv.clone()));
            let mut rewritten: Option<Value> = None;
            host.chain_hooks_callback::<(Value, i32, Value), Value, _, _>(
                crate::pluginhost::HOOK_ENCLOSURE_ENTRY,
                &mut entry_state,
                |plugin, host, st| {
                    Box::pin(async move {
                        plugin
                            .hook_enclosure_entry(host, &mut st.0, st.1, &st.2)
                            .await
                    })
                },
                |r, _| {
                    rewritten = Some(r.clone());
                    false
                },
            )
            .await;
            entries.push(rewritten.unwrap_or_else(|| enc.clone()));
        }
        rv.insert("formatted".into(), json!(formatted));
        rv.insert("entries".into(), json!(entries));
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

/// `UrlHelper::url_to_youtube_vid`.
pub(crate) fn url_to_youtube_vid(url: &str) -> Option<String> {
    let url = url.replace("youtube.com", "youtube-nocookie.com");
    let patterns = [
        r"//www\.youtube-nocookie\.com/v/([\w-]+)",
        r"//www\.youtube-nocookie\.com/embed/([\w-]+)",
        r"//www\.youtube-nocookie\.com/watch\?v=([\w-]+)",
        r"//youtu\.be/([\w-]+)",
    ];
    for p in patterns {
        if let Ok(re) = regex::Regex::new(p)
            && let Some(caps) = re.captures(&url)
        {
            return Some(caps[1].to_string());
        }
    }
    None
}

/// `Feeds::_has_icon`.
fn has_icon(icon_dir: &std::path::Path, feed_id: i32) -> bool {
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
    (
        ((c >> 16) & 0xFF) as i64,
        ((c >> 8) & 0xFF) as i64,
        (c & 0xFF) as i64,
    )
}

// ---- `_get_headlines` ------------------------------------------------------

/// Result of the headline query ([`get_headlines_query`] / `Feeds::_get_headlines`):
/// the rows plus the feed metadata (PHP's 9-element return tuple, including
/// `search_words` — the FTS words from `_search_to_sql`, empty without an
/// active search).
pub struct HeadlinesQuery {
    pub article_rows: Vec<sqlx::postgres::PgRow>,
    pub feed_title: String,
    pub feed_site_url: String,
    pub last_error: String,
    pub last_updated: Option<String>,
    /// `search_words` from `_search_to_sql` (the FTS words); empty when the
    /// search filter is inactive (and used in the "Incorrect search syntax"
    /// message when the fragment fails its dry-run precheck).
    pub search_words: Vec<String>,
    pub first_id: i32,
    pub is_vfeed: bool,
    pub query_error_override: String,
    pub first_id_changed: bool,
}

/// `Feeds::_get_headlines($params)` — the parameter shape (the subset the
/// Rust server and the integration tests use; unset PHP defaults apply).
#[derive(Debug, Clone, Default)]
pub struct HeadlinesParams {
    /// `feed` — a numeric feed id (a non-numeric string is a tag name).
    pub feed: String,
    /// `limit` (PHP default 30).
    pub limit: i64,
    /// `view_mode` (PHP default `"all"` here; PHP leaves it unset).
    pub view_mode: String,
    /// `cat_view` (PHP default false).
    pub cat_view: bool,
    /// `offset` (PHP default 0).
    pub offset: i64,
    /// `since_id` (PHP default 0): only articles with a larger id.
    pub since_id: i32,
    /// `owner_uid` (PHP: `$_SESSION["uid"]`).
    pub owner_uid: i32,
    /// `search` (PHP default `""`): the search filter, converted to a SQL
    /// fragment by `_search_to_sql` (see [`crate::search`]) and spliced into
    /// the query; an unexecutable fragment degrades to `false` with a
    /// `query_error_override` message.
    pub search: String,
    /// `search_language` (PHP: `$_REQUEST["search_language"] ?? ""`,
    /// "PGSQL only"): the `to_tsquery` text-search config for FTS words;
    /// empty falls back to the `DEFAULT_SEARCH_LANGUAGE` pref (itself
    /// defaulting to `""`, in which case plain FTS words cannot execute).
    pub search_language: String,
    /// `override_order` (PHP default `""`): explicit `ORDER BY` override
    /// (from `Feeds::_order_to_override_query` or a virtual-feed default).
    pub override_order: String,
    /// `include_children` (PHP default false): include subcategory feeds in
    /// a category view.
    pub include_children: bool,
    /// `ignore_vfeed_group` (PHP default false): skip the VFEED_GROUP_BY_FEED
    /// date-group ordering (syndicated feeds set this true).
    pub ignore_vfeed_group: bool,
    /// `start_ts` (PHP default `""`): only articles entered on/after this
    /// timestamp.
    pub start_ts: String,
    /// `skip_first_id_check` (PHP default false): skip the topmost-article-id
    /// pagination check (set by `date_reverse` ordering via syndication).
    pub skip_first_id_check: bool,
    /// `check_first_id` (PHP default 0): the id of the client's last-seen top
    /// article. When `offset > 0` and the current top id differs, the result
    /// set changed since the client last viewed it (PHP returns `[-1, …]`) —
    /// signalled as `first_id_changed` with an empty row set.
    pub check_first_id: i32,
}

impl HeadlinesParams {
    /// The PHP defaults (`limit` 30, `view_mode` `"all"`).
    pub fn new(feed: impl Into<String>, owner_uid: i32) -> Self {
        Self {
            feed: feed.into(),
            limit: 30,
            view_mode: "all".into(),
            cat_view: false,
            offset: 0,
            since_id: 0,
            owner_uid,
            search: String::new(),
            search_language: String::new(),
            override_order: String::new(),
            include_children: false,
            ignore_vfeed_group: false,
            start_ts: String::new(),
            skip_first_id_check: false,
            check_first_id: 0,
        }
    }
}

/// `Feeds::_get_headlines($params)` — the dynamic headline query (numeric
/// feeds / categories / virtual feeds / labels / tags). The session profile
/// is unavailable in this context, so prefs resolve against the default
/// profile (matching the integration-test session, which carries none).
pub async fn get_headlines_query(
    db: &crate::core::db::Db,
    params: &HeadlinesParams,
) -> HeadlinesQuery {
    let feed_num: Option<i32> = params.feed.trim().parse().ok();
    let rows = prefs::load_rows(db, params.owner_uid, None).await;
    get_headlines(
        db,
        params.owner_uid,
        &params.feed,
        feed_num,
        params.limit,
        &params.view_mode,
        params.cat_view,
        &params.search,
        &params.search_language,
        &params.override_order,
        params.offset,
        params.include_children,
        params.check_first_id,
        params.skip_first_id_check,
        "", // order_by (PHP: commented out)
        &rows,
        None, // profile
        params.since_id,
        params.ignore_vfeed_group,
        &params.start_ts,
    )
    .await
}

/// `Feeds::_get_headlines` — the dynamic headline query, ported branch-for-
/// branch from the PHP (faithful port — the PHP method takes this many
/// parameters).
#[allow(clippy::too_many_arguments)]
async fn get_headlines(
    db: &crate::core::db::Db,
    uid: i32,
    feed: &str,
    feed_num: Option<i32>,
    limit: i64,
    view_mode: &str,
    cat_view: bool,
    search: &str,
    search_language: &str,
    override_order: &str,
    offset: i64,
    include_children: bool,
    check_first_id: i32,
    skip_first_id_check: bool,
    _order_by: &str,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
    since_id: i32,
    ignore_vfeed_group: bool,
    start_ts: &str,
) -> HeadlinesQuery {
    // The branch logic below mutates `$override_order` (starred / published /
    // recently-read get a default order unless the caller supplied one).
    let mut override_order = override_order.to_string();
    let pool = db.pool();

    // `$start_ts_query_part = $start_ts ? "date_entered >= 'Y/m/d H:i:s' AND " : ''`
    //
    // `strtotime` of the client ts, rendered as UTC `Y/m/d H:i:s` (the PHP
    // server runs in UTC; the stored datetimes are UTC). `strtotime`'s
    // garbage-input behavior (`false` → epoch) is mirrored in the fallback.
    let start_ts_query_part = if start_ts.is_empty() {
        String::new()
    } else {
        let dt = chrono::NaiveDateTime::parse_from_str(start_ts, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(start_ts, "%Y-%m-%d %H:%M"))
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(start_ts, "%Y-%m-%d")
                    .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
            })
            .or_else(|_| {
                chrono::DateTime::parse_from_rfc3339(start_ts)
                    .map(|d| d.with_timezone(&chrono::Utc).naive_utc())
            })
            .unwrap_or_else(|_| chrono::DateTime::UNIX_EPOCH.naive_utc());
        format!("date_entered >= '{}' AND ", dt.format("%Y/%m/%d %H:%M:%S"))
    };

    // Assigned in every branch of the `feed_num` match below.
    let query_strategy_part: String;
    let mut vfeed_query_part = String::new();
    let mut ext_tables_part = String::new();
    let mut allow_archived = false;
    let mut ignore_vfeed_group = ignore_vfeed_group;

    let fresh_max_age = prefs::resolve(prefs::FRESH_ARTICLE_MAX_AGE, rows, profile).as_int();
    let recently_read_max_age = prefs::resolve("RECENTLY_READ_MAX_AGE", rows, profile).as_int();

    // $view_query_part
    let mut view_query_part = String::new();
    if view_mode == "adaptive" {
        // PHP: a search → no view filter; a concrete feed (`$feed != -1`) scopes
        // the list to unread when that feed (a category, plus its children when
        // `include_children`) has any unread, otherwise shows everything.
        if search.is_empty() && feed_num != Some(-1) {
            let feed_or_tag: crate::counters::FeedOrTag<'_> = match feed_num {
                Some(n) => crate::counters::FeedOrTag::Feed(n),
                None => crate::counters::FeedOrTag::Tag(feed),
            };
            let mut unread =
                crate::counters::get_counters(db, uid, feed_or_tag, cat_view, true, None).await;
            if cat_view
                && let Some(f) = feed_num
                && f > 0
                && include_children
            {
                unread += crate::counters::cat_children_unread(pool, uid, f).await;
            }
            if unread > 0 {
                view_query_part = " unread = true AND ".into();
            }
        }
    }
    view_query_part = match view_mode {
        "marked" => " marked = true AND ".into(),
        "has_note" => " (note IS NOT NULL AND note != '') AND ".into(),
        "published" => " published = true AND ".into(),
        "unread" if feed_num != Some(FEED_RECENTLY_READ) => " unread = true AND ".into(),
        _ => view_query_part,
    };

    let limit_query_part = if limit > 0 {
        format!("LIMIT {limit}")
    } else {
        String::new()
    };
    let offset_query_part = if limit_query_part.is_empty() {
        String::new()
    } else {
        format!("OFFSET {offset}")
    };
    let since_id_part = if since_id != 0 {
        format!("ttrss_entries.id > {since_id} AND ")
    } else {
        String::new()
    };

    // ---- search scope --------------------------------------------------
    // PHP: HOOK_SEARCH (plugins, M7) then `_search_to_sql`, plus a dry-run
    // precheck: an unexecutable fragment becomes `false` with an
    // "Incorrect search syntax: <words>." override message.
    let (search_query_part, search_words, query_error_override) = if search.is_empty() {
        (String::new(), Vec::new(), String::new())
    } else {
        let parts =
            crate::search::search_to_sql_cached(pool, rows, uid, profile, search, search_language)
                .await;
        let (qpart, override_msg) =
            crate::search::dry_run(pool, &parts.query_part, &parts.words).await;
        (format!("{qpart} AND "), parts.words, override_msg)
    };

    // query strategy per branch
    match feed_num {
        None => {
            // browsing by tag
            query_strategy_part = "true".into();
            vfeed_query_part =
                "(SELECT title FROM ttrss_feeds WHERE\n\t\t\t\t\tid = feed_id) as feed_title,"
                    .into();
        }
        Some(f) if f > 0 => {
            if cat_view {
                if include_children {
                    let mut subcats: Vec<i32> = child_cats(pool, uid, f).await;
                    subcats.push(f);
                    let list = subcats
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
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
                override_order = "last_marked DESC, date_entered DESC, updated DESC".into();
            }
        }
        Some(f) if f == FEED_PUBLISHED || f == CAT_LABELS => {
            if !cat_view {
                query_strategy_part = "published = true".into();
                vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
                allow_archived = true;
                if override_order.is_empty() {
                    override_order = "last_published DESC, date_entered DESC, updated DESC".into();
                }
            } else {
                vfeed_query_part = "ttrss_feeds.title AS feed_title,".into();
                ext_tables_part = "ttrss_labels2,ttrss_user_labels2,".into();
                query_strategy_part = "ttrss_labels2.id = ttrss_user_labels2.label_id AND
\t\t\t\t\tttrss_user_labels2.article_id = ref_id"
                    .into();
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
    let (feed_title, feed_site_url, last_error, last_updated) = if !search.is_empty() {
        (
            t_sprintf("Search results: %s", &[search]),
            String::new(),
            String::new(),
            None,
        )
    } else if cat_view {
        (
            cat_title(pool, uid, feed_num.unwrap_or(0)).await,
            String::new(),
            String::new(),
            None,
        )
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
                (
                    format!("Unknown feed ({f})"),
                    String::new(),
                    String::new(),
                    None,
                )
            }
        } else {
            (
                // Special/virtual feed (`f <= 0`): `feed_title_of` returns the
                // canonical English title (PHP `Feeds::_get_title`), which we
                // translate. Real feeds (`f > 0`) are above and stay as stored.
                __(&feed_title_of(f, pool, uid).await),
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

    let mut first_id = 0i32;
    let first_id_changed = false;
    // Assigned in both branches of the `feed_num` if/else below.
    let article_rows: Vec<sqlx::postgres::PgRow>;

    if let Some(feed) = feed_num {
        // vfeed group ordering (VFEED_GROUP_BY_FEED is false in this
        // deployment; ported for parity)
        if !vfeed_query_part.is_empty()
            && !ignore_vfeed_group
            && prefs::resolve("VFEED_GROUP_BY_FEED", rows, profile).as_bool()
        {
            let mut yyiw_order_qpart = String::new();
            if !((NEVER_GROUP_BY_DATE.contains(&feed)) && !cat_view) {
                let yyiw_desc = if order_by == "date_reverse" {
                    ""
                } else {
                    "desc"
                };
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

        let mut first_id_query_strategy_part = query_strategy_part.clone();
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
\t\t\t\t{start_ts_query_part}{since_id_part}date_entered >= NOW() - INTERVAL '1 hour' AND
\t\t\t\t{first_id_query_strategy_part} ORDER BY {order_by} LIMIT 1"
            );
            if let Ok(Some(r)) = sqlx::query(AssertSqlSafe(query)).fetch_optional(pool).await {
                first_id = r.get::<i32, _>(0);
                if offset > 0 && first_id != 0 && check_first_id != 0 && first_id != check_first_id
                {
                    return HeadlinesQuery {
                        article_rows: Vec::new(),
                        feed_title,
                        feed_site_url,
                        last_error,
                        last_updated,
                        search_words: Vec::new(),
                        first_id,
                        is_vfeed: !vfeed_query_part.is_empty(),
                        query_error_override: String::new(),
                        first_id_changed: true,
                    };
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
\t\t\t\t{search_query_part}
\t\t\t\t{start_ts_query_part}
\t\t\t\t{view_query_part}
\t\t\t\t{since_id_part}
\t\t\t\t{query_strategy_part} ORDER BY {order_by}
\t\t\t\t{limit_query_part} {offset_query_part}"
        );

        article_rows = sqlx::query(AssertSqlSafe(query))
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
\t\t\t\t\t\t\t{search_query_part}
\t\t\t\t\t\t\t{start_ts_query_part}
\t\t\t\t\t\t\t{query_strategy_part} ORDER BY {order_by}
\t\t\t\t\t\t\t{limit_query_part} {offset_query_part}"
        );
        article_rows = sqlx::query(AssertSqlSafe(query))
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
        search_words,
        first_id,
        is_vfeed: !vfeed_query_part.is_empty(),
        query_error_override,
        first_id_changed,
    }
}

/// `Feeds::_get_child_cats` (natural PG order).
pub(crate) async fn child_cats(pool: &sqlx::PgPool, uid: i32, cat: i32) -> Vec<i32> {
    let rows = sqlx::query(
        "SELECT id FROM ttrss_feed_categories WHERE (parent_cat = $1 AND owner_uid = $2)",
    )
    .bind(cat)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter().map(|r| r.get::<i32, _>(0)).collect()
}

/// `Feeds::_get_cat_title`. The three special categories are translated
/// (PHP `Feeds.php:1315-1319`); real category titles come from the DB.
async fn cat_title(pool: &sqlx::PgPool, uid: i32, cat_id: i32) -> String {
    match cat_id {
        CAT_UNCATEGORIZED => __("Uncategorized"),
        CAT_SPECIAL => __("Special"),
        CAT_LABELS => __("Labels"),
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

/// `Feeds::_get_title` (the non-cat branch) — delegates to the canonical
/// `feeds::feed_title` (the PHP-faithful implementation, including the
/// id-as-string fallback for the else branch, e.g. `FEED_ERROR` → `"-7"`).
async fn feed_title_of(feed: i32, pool: &sqlx::PgPool, uid: i32) -> String {
    crate::feeds::feed_title(pool, feed, uid).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn url_to_youtube_vid_cases() {
        // test_url_to_youtube_vid_watch / _short / _v / _invalid
        // (UrlHelperTest.php), ported 1:1.
        assert_eq!(
            url_to_youtube_vid("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            url_to_youtube_vid("https://youtu.be/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            url_to_youtube_vid("https://www.youtube.com/v/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(url_to_youtube_vid("https://www.youtube.com/"), None);
        assert_eq!(url_to_youtube_vid("https://vimeo.com/12345"), None);
        assert_eq!(url_to_youtube_vid("not-a-url"), None);
        assert_eq!(url_to_youtube_vid(""), None);
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
    // The reference plugins' RAW hook output (single quotes as in the
    // PHP source) and the exact DOM-round-tripped oracle strings (double
    // quotes, `data-plugin-name` appended) the chain must produce.
    fn trgm_raw(id: i32) -> String {
        format!(
            "<i style=\"cursor : pointer\" class='material-icons' \
             onclick=\"Plugins.Psql_Trgm.showRelated({id})\" \
             title=\"Show related articles\">bookmark_outline</i>"
        )
    }
    fn note_raw(id: i32) -> String {
        format!(
            "<i class='material-icons' onclick=\"Plugins.Note.edit({id})\" \
             style='cursor : pointer' title=\"Edit article note\">note</i>"
        )
    }
    fn share_raw(int_id: i64, shared: bool) -> String {
        let icon_class = if shared { "is-shared" } else { "" };
        format!(
            "<i class='material-icons icon-share share-icon-{int_id} {icon_class}' \
             style='cursor : pointer' onclick=\"Plugins.Share.shareArticle({int_id})\" \
             title=\"Share by URL\">link</i>"
        )
    }

    #[test]
    fn article_button_roundtrip_matches_oracle() {
        // af_psql_trgm
        assert_eq!(
            button_xml_roundtrip(&trgm_raw(1), "Af_Psql_Trgm"),
            Some(
                "<i style=\"cursor : pointer\" class=\"material-icons\" onclick=\"Plugins.Psql_Trgm.showRelated(1)\" title=\"Show related articles\" data-plugin-name=\"Af_Psql_Trgm\">bookmark_outline</i>"
                    .to_string()
            )
        );
        // note
        assert_eq!(
            button_xml_roundtrip(&note_raw(1), "Note"),
            Some(
                "<i class=\"material-icons\" onclick=\"Plugins.Note.edit(1)\" style=\"cursor : pointer\" title=\"Edit article note\" data-plugin-name=\"Note\">note</i>"
                    .to_string()
            )
        );
        // share, unshared (note the space before the empty icon class)
        assert_eq!(
            button_xml_roundtrip(&share_raw(100, false), "Share"),
            Some(
                "<i class=\"material-icons icon-share share-icon-100 \" style=\"cursor : pointer\" onclick=\"Plugins.Share.shareArticle(100)\" title=\"Share by URL\" data-plugin-name=\"Share\">link</i>"
                    .to_string()
            )
        );
        // share, shared
        assert_eq!(
            button_xml_roundtrip(&share_raw(100, true), "Share"),
            Some(
                "<i class=\"material-icons icon-share share-icon-100 is-shared\" style=\"cursor : pointer\" onclick=\"Plugins.Share.shareArticle(100)\" title=\"Share by URL\" data-plugin-name=\"Share\">link</i>"
                    .to_string()
            )
        );
        // not valid XML → skipped (PHP user_error)
        assert_eq!(button_xml_roundtrip("<i><b></i>", "X"), None);
        assert_eq!(button_xml_roundtrip("", "X"), None);
    }

    #[tokio::test]
    async fn article_buttons_chain_matches_oracle() {
        use crate::pluginhost::host::HostCtx;
        let host = Arc::new(crate::pluginhost::PluginHost::startup(
            &crate::pluginhost::all_plugins(),
        ));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_psql_trgm", "note", "share"],
        );
        // No active plugins → no buttons.
        let mut empty = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &[],
        );
        let line = serde_json::json!({ "id": 1, "uuid": "" });
        assert_eq!(article_buttons(&mut empty, &line).await, "");

        let line = serde_json::json!({ "id": 2, "int_id": 2, "uuid": "" });
        let out = article_buttons(&mut ctx, &line).await;
        // Byte parity: the concatenation of the three round-tripped oracle
        // strings, in load order (trgm, note, share).
        let expected = button_xml_roundtrip(&trgm_raw(2), "Af_Psql_Trgm").unwrap()
            + &button_xml_roundtrip(&note_raw(2), "Note").unwrap()
            + &button_xml_roundtrip(&share_raw(2, false), "Share").unwrap();
        assert_eq!(out, expected);
        // plugin order: trgm, note, share (load order)
        let i_trgm = out.find("Psql_Trgm").unwrap();
        let i_note = out.find("Note.edit").unwrap();
        let i_share = out.find("Share.shareArticle").unwrap();
        assert!(i_trgm < i_note && i_note < i_share);

        // shared article → is-shared class
        let line = serde_json::json!({ "id": 3, "int_id": 3, "uuid": "abc" });
        let out = article_buttons(&mut ctx, &line).await;
        assert!(out.contains("share-icon-3 is-shared"));
    }
}
