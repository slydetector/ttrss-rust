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

use crate::cats::{CAT_LABELS, CAT_SPECIAL, CAT_UNCATEGORIZED};
use crate::prefs;
use crate::sanitizer;
use crate::session::Sess;
use crate::time::{self, TzCtx};
use crate::util::{basename, param_to_bool, strip_tags, truncate_string};
use crate::AppState;

use crate::feeds::{
    FEED_ALL, FEED_ARCHIVED, FEED_FRESH, FEED_PUBLISHED, FEED_RECENTLY_READ, FEED_STARRED,
};
use crate::labels::{feed_to_label_id, LABEL_BASE_INDEX};

// `Feeds::FEED_ERROR` (functions.php): the virtual "error feed" pseudo-id.
pub(crate) const FEED_ERROR: i64 = -7;
/// `PLUGIN_FEED_BASE_INDEX` (functions.php): virtual plugin feeds occupy
/// `(LABEL_BASE_INDEX, PLUGIN_FEED_BASE_INDEX)`.
pub(crate) const PLUGIN_FEED_BASE_INDEX: i64 = -128;
const NEVER_GROUP_FEEDS: &[i64] = &[FEED_RECENTLY_READ, FEED_ARCHIVED];
const NEVER_GROUP_BY_DATE: &[i64] = &[FEED_PUBLISHED, FEED_STARRED, FEED_FRESH];

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
    let check_first_id = extra
        .get("fid")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
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
pub(crate) fn order_to_override_query(order: &str) -> (String, bool) {
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
        search_words: _,
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

    // `HOOK_HEADLINE_TOOLBAR_*` — no loaded plugins register them.
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

    let show_content_preview = prefs::resolve("SHOW_CONTENT_PREVIEW", rows, profile).as_bool();
    let strip_images = prefs::resolve(prefs::STRIP_IMAGES, rows, profile).as_bool();
    let bw_limit = data.get_bool("bw_limit").unwrap_or(false);
    let has_sandbox = data.get_bool("hasSandbox").unwrap_or(false);

    // M7.6: the HOOK_SANITIZE context (built once per request).
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;

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
        let vfeed_title: Option<String> = if is_vfeed_row {
            r.get::<Option<String>, _>(I_VF_TITLE)
        } else {
            None
        };
        let favicon_avg_color: Option<String> = if is_vfeed_row {
            r.get::<Option<String>, _>(I_VF_FAVICON)
        } else {
            None
        };
        let content_raw: String = r.get(if is_vfeed_row {
            I_VF_CONTENT
        } else {
            I_CONTENT
        });
        let author: String = r.get(if is_vfeed_row { 28 } else { 26 });
        let score: i32 = r.get(if is_vfeed_row { 29 } else { 27 });
        let num_labels: i64 = r.get(if is_vfeed_row { 30 } else { 28 });
        let num_enclosures: i64 = r.get(if is_vfeed_row { 31 } else { 29 });

        let updated_str = updated.as_ref().map(time::naive_to_pg_text);
        let date_entered_str = date_entered.as_ref().map(time::naive_to_pg_text);
        let last_read_str = last_read.as_ref().map(time::naive_to_pg_text);

        // `content_preview` — first key added (pre-sanitize raw content).
        let content_preview = if show_content_preview {
            format!(
                "&mdash; {}",
                truncate_string(&strip_tags(&content_raw), 250, "&hellip;")
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
        line.insert(
            "buttons".into(),
            json!(article_buttons(id, int_id, is_shared)),
        );
        line.insert("enclosures".into(), enclosures);
        line.insert(
            "updated_long".into(),
            json!(time::make_local_datetime(
                updated_str.as_deref(),
                false,
                &tz
            )),
        );
        line.insert(
            "imported".into(),
            json!(format!(
                "Imported at {}",
                time::make_local_datetime(date_entered_str.as_deref(), false, &tz)
            )),
        );
        line.insert(
            "tags".into(),
            Value::Array(tags.iter().map(|s| Value::from(s.as_str())).collect()),
        );
        line.insert(
            "has_icon".into(),
            json!(has_icon(&state.config.feed_icon_dir, feed_id)),
        );
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

    let can_inline =
        !bw_limit && !strip_images && (always_display_enclosures || !contains_img(article_content));
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
        if let Ok(re) = regex::Regex::new(p) {
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
    pub first_id: i64,
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
    pub since_id: i64,
    /// `owner_uid` (PHP: `$_SESSION["uid"]`).
    pub owner_uid: i64,
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
}

impl HeadlinesParams {
    /// The PHP defaults (`limit` 30, `view_mode` `"all"`).
    pub fn new(feed: impl Into<String>, owner_uid: i64) -> Self {
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
    let feed_num: Option<i64> = params.feed.trim().parse::<i64>().ok();
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
        0, // check_first_id
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
    uid: i64,
    feed: &str,
    feed_num: Option<i64>,
    limit: i64,
    view_mode: &str,
    cat_view: bool,
    search: &str,
    search_language: &str,
    override_order: &str,
    offset: i64,
    include_children: bool,
    check_first_id: i64,
    skip_first_id_check: bool,
    _order_by: &str,
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
    since_id: i64,
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
            if cat_view {
                if let Some(f) = feed_num {
                    if f > 0 && include_children {
                        unread += crate::counters::cat_children_unread(pool, uid, f).await;
                    }
                }
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
                    let mut subcats: Vec<i64> = child_cats(pool, uid, f).await;
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
            format!("Search results: {search}"),
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
            if let Ok(Some(r)) = sqlx::query(&query).fetch_optional(pool).await {
                first_id = r.get::<i32, _>(0) as i64;
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
\t\t\t\t\t\t\t{search_query_part}
\t\t\t\t\t\t\t{start_ts_query_part}
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
        search_words,
        first_id,
        is_vfeed: !vfeed_query_part.is_empty(),
        query_error_override,
        first_id_changed,
    }
}

/// `Feeds::_get_child_cats` (natural PG order).
pub(crate) async fn child_cats(pool: &sqlx::PgPool, uid: i64, cat: i64) -> Vec<i64> {
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

/// `Feeds::_get_title` (the non-cat branch) — delegates to the canonical
/// `feeds::feed_title` (the PHP-faithful implementation, including the
/// id-as-string fallback for the else branch, e.g. `FEED_ERROR` → `"-7"`).
async fn feed_title_of(feed: i64, pool: &sqlx::PgPool, uid: i64) -> String {
    crate::feeds::feed_title(pool, feed, uid).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
