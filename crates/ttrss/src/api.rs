//! Port of `api/index.php` + the `API` handler (M2 external API reads).
//!
//! The external JSON API (`/api/index.php`) speaks a `{seq, status, content}`
//! envelope and authenticates with **app passwords** (`service = '_api'`).
//! Sessions are addressed by a `sid` field in the request body (not the web
//! cookie), so the handler re-points the session handle at the `sid` session.
//!
//! This module ports the entry point (content-type gate, `sid` session,
//! `before()` auth) and the read-only level-23 methods the corpus exercises
//! (`login`, `isloggedin`, `getConfig`, `getUnread`, `getCounters`,
//! `getFeeds`, `getCategories`, `getFeedTree`, `getLabels`, `getHeadlines`,
//! `getArticle`, `getPref`). Mutating methods and the plugin-API fallback land
//! in later milestones.

use crate::core::phpser::Value as PValue;
use crate::core::user;
use axum::body::Body;
use axum::extract::{Extension, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::article;
use crate::auth;
use crate::cats::{CAT_ALL, CAT_ALL_EXCEPT_VIRTUAL, CAT_LABELS, CAT_SPECIAL, CAT_UNCATEGORIZED};
use crate::counters;
use crate::feeds;
use crate::headlines;
use crate::labels;
use crate::prefs;
use crate::sanitizer;
use crate::session::{self, Sess};
use crate::util::{
    clean_int, clean_str, param_int_array, param_to_bool, php_int_cast, strip_tags, truncate_string,
};
use crate::AppState;

const API_LEVEL: i64 = 23;
const STATUS_OK: i64 = 0;
const STATUS_ERR: i64 = 1;
const E_API_DISABLED: &str = "API_DISABLED";
const E_NOT_LOGGED_IN: &str = "NOT_LOGGED_IN";
const E_LOGIN_ERROR: &str = "LOGIN_ERROR";
const E_INCORRECT_USAGE: &str = "INCORRECT_USAGE";
const E_UNKNOWN_METHOD: &str = "UNKNOWN_METHOD";
const E_OPERATION_FAILED: &str = "E_OPERATION_FAILED";
const E_NOT_FOUND: &str = "E_NOT_FOUND";
/// `Auth_Base::AUTH_SERVICE_API` — the app-password service token.
const AUTH_SERVICE_API: &str = "_api";

/// `api/index.php` — the external JSON API entry point.
pub async fn handler(state: State<AppState>, sess: Extension<Sess>, req: Request) -> Response {
    // `api/index.php`: reject a non-JSON `Content-Type` up front.
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type_is_json(ct) {
        return envelope(-1, STATUS_ERR, json!({ "error": E_INCORRECT_USAGE })).into_response();
    }

    let bytes = axum::body::to_bytes(req.into_body(), 1_048_576)
        .await
        .unwrap_or_default();
    let body: Map<String, Value> = serde_json::from_slice(&bytes).unwrap_or_default();

    // `$_REQUEST["sid"]` → load and adopt the addressed session (the API client
    // carries no cookie; `session_id($_REQUEST["sid"]); session_start()`).
    if let Some(sid) = body.get("sid").and_then(Value::as_str) {
        if !sid.is_empty() {
            let data = session::load(&state.db, state.config.encryption_key, sid).await;
            sess.replace(sid.to_string(), data);
        }
    }

    let op = body
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let seq = clean_int(body.get("seq"));

    // `getFeedIcon` is the one API method that returns raw bytes (a cached
    // favicon image), not the JSON envelope — route it to a raw response.
    if op == "getfeedicon" {
        return get_feed_icon(&state, &sess, &body, seq).await;
    }
    let (status, content) = before_and_dispatch(&state, &sess, &op, &body).await;

    let mut res = envelope(seq, status, content);
    // `before()` sets `Content-Type: application/json`; the session `nocache`
    // limiter applies when a session was started.
    if sess.had_cookie() {
        crate::nocache_headers(res.headers_mut());
    }
    res
}

/// The `API::before()` gate: verify the session is authenticated and the
/// external API is enabled. Returns `Ok(())` when the call may proceed, else
/// the `(status, content)` error envelope to return.
async fn api_auth(state: &AppState, sess: &Sess, op: &str) -> Result<(), (i64, Value)> {
    let uid = sess.data().uid();
    if let Some(u) = uid {
        let rows = prefs::load_rows(&state.db, u, None).await;
        if op != "logout" && !prefs::resolve("ENABLE_API_ACCESS", &rows, None).as_bool() {
            return Err((STATUS_ERR, json!({ "error": E_API_DISABLED })));
        }
        Ok(())
    } else if matches!(op, "login" | "isloggedin" | "logout") {
        Ok(())
    } else {
        Err((STATUS_ERR, json!({ "error": E_NOT_LOGGED_IN })))
    }
}
/// The `API::before()` gate + method dispatch, folded into one pass so the
/// `seq`/`status`/`content` triple is built in a single place.
pub async fn before_and_dispatch(
    state: &AppState,
    sess: &Sess,
    op: &str,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let data = sess.data();
    let uid = data.uid();

    if let Err((status, content)) = api_auth(state, sess, op).await {
        return (status, content);
    }

    match op {
        "login" => login(state, sess, body).await,
        // `API::logout`: destroy the session; the middleware deletes the stored
        // row. `UserHelper::logout` always succeeds, so no login gate is needed.
        "logout" => {
            sess.destroy();
            (STATUS_OK, json!({ "status": "OK" }))
        }
        "isloggedin" => (STATUS_OK, json!({ "status": data.uid().is_some() })),
        "getconfig" => (STATUS_OK, get_config(state, uid).await),
        "getunread" => get_unread(state, uid, body).await,
        "getcounters" => get_counters(state, sess, body).await,
        "getfeeds" => get_feeds(state, uid, body).await,
        "getcategories" => get_categories(state, uid, body).await,
        "getfeedtree" => get_feed_tree(state, sess, body).await,
        "getlabels" => get_labels(state, uid, body).await,
        "getheadlines" => get_headlines(state, sess, body).await,
        "getarticle" => get_article(state, sess, uid, body).await,
        "getpref" => get_pref(state, sess, body).await,
        "getversion" => (
            STATUS_OK,
            json!({ "version": crate::core::config::VERSION }),
        ),
        "getapilevel" => (STATUS_OK, json!({ "level": API_LEVEL })),
        "catchupfeed" => catchup_feed(state, sess, body).await,
        "unsubscribefeed" => unsubscribe_feed(state, sess, body).await,
        "updatefeed" => update_feed(state, sess, body).await,
        "subscribetofeed" => subscribe_to_feed(state, sess, body).await,
        "updatearticle" => update_article(state, sess, body).await,
        "setarticlelabel" => set_article_label(state, sess, body).await,
        "sharetopublished" => share_to_published(state, sess, body).await,
        other => (
            STATUS_ERR,
            json!({ "error": E_UNKNOWN_METHOD, "method": other }),
        ),
    }
}

/// `{seq, status, content}` JSON envelope (`API::_wrap`).
fn envelope(seq: i64, status: i64, content: Value) -> Response {
    let body = json!({ "seq": seq, "status": status, "content": content }).to_string();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("valid response")
}

/// PHP `%^(?:application|text)/json(?:\s*;|$)%i`.
fn content_type_is_json(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim();
    let base = base.to_ascii_lowercase();
    base == "application/json" || base == "text/json"
}

/// Like `param_to_bool`, but falls back to `default` when the key is absent
/// (PHP `clean($_REQUEST[$k] ?? $default)`).
fn param_bool_default(body: &Map<String, Value>, key: &str, default: bool) -> bool {
    match body.get(key) {
        Some(v) => param_to_bool(Some(v)),
        None => default,
    }
}
// ── Login / config ──────────────────────────────────────────────────────────

/// `API::login` — app-password auth (`service = '_api'`), then a fresh session.
async fn login(state: &AppState, sess: &Sess, body: &Map<String, Value>) -> (i64, Value) {
    let pool = state.db.pool();
    let login_name = clean_str(body.get("user").and_then(Value::as_str).unwrap_or(""));
    let password = clean_str(body.get("password").and_then(Value::as_str).unwrap_or(""));

    // `UserHelper::find_user_by_login` (LOWER(login) = LOWER(?)).
    let row = sqlx::query(
        "SELECT id, login, access_level, pwd_hash, otp_enabled
         FROM ttrss_users WHERE LOWER(login) = LOWER($1)",
    )
    .bind(&login_name)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(r) = row else {
        return (STATUS_ERR, json!({ "error": E_LOGIN_ERROR }));
    };
    let uid: i32 = r.get("id");
    let u_login: String = r.get("login");
    let access_level: i32 = r.get("access_level");
    let pwd_hash: String = r.get("pwd_hash");
    let otp_enabled: bool = r.get("otp_enabled");

    if access_level == user::ACCESS_LEVEL_DISABLED {
        return (STATUS_ERR, json!({ "error": E_LOGIN_ERROR }));
    }

    // `Prefs::get(Prefs::ENABLE_API_ACCESS, $uid)` (profile-blacklisted → default).
    let rows = prefs::load_rows(&state.db, uid as i64, None).await;
    if !prefs::resolve("ENABLE_API_ACCESS", &rows, None).as_bool() {
        return (STATUS_ERR, json!({ "error": E_API_DISABLED }));
    }

    // `auth_internal::authenticate` (service = '_api'): app passwords are tried
    // first; the *regular* password is a fallback only when OTP is **not**
    // enabled (an OTP-enabled account must use an app password for service
    // logins). Mirrors `Auth_Internal::authenticate`'s service-login path.
    let ok_app_password = verify_app_password(pool, &login_name, &password).await;
    let ok_main_password =
        if !ok_app_password && !otp_enabled {
            // `Auth_Internal::check_password` (service = "") fallback.
            auth::check_password(pool, uid, &password).await
        } else {
            false
        };
    if !ok_app_password && !ok_main_password {
        return (STATUS_ERR, json!({ "error": E_LOGIN_ERROR }));
    }

    // `UserHelper::authenticate` success side-effects: stamp last_login, build a
    // fresh session (`session_regenerate_id(true)`).
    let _ = sqlx::query("UPDATE ttrss_users SET last_login = NOW() WHERE id = $1")
        .bind(uid)
        .execute(pool)
        .await;
    let (new_id, data) =
        auth::build_login_session(uid, &u_login, access_level, &pwd_hash, false, false);
    sess.replace(new_id.clone(), data);

    (
        STATUS_OK,
        json!({
            "session_id": new_id,
            "config": get_config(state, Some(uid as i64)).await,
            "api_level": API_LEVEL,
        }),
    )
}

/// `auth_internal::check_app_password`: verify a stored `ALGO:HASH:SALT` app
/// password for `service`, stamping `last_used` on a match.
async fn verify_app_password(pool: &sqlx::PgPool, login: &str, password: &str) -> bool {
    let rows = sqlx::query(
        "SELECT p.id, p.pwd_hash
         FROM ttrss_app_passwords p, ttrss_users u
         WHERE p.owner_uid = u.id AND LOWER(u.login) = LOWER($1) AND p.service = $2",
    )
    .bind(login)
    .bind(AUTH_SERVICE_API)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for r in &rows {
        let stored: String = r.get("pwd_hash");
        if user::verify_app_password(&stored, password) {
            let id: i32 = r.get("id");
            let _ = sqlx::query("UPDATE ttrss_app_passwords SET last_used = NOW() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await;
            return true;
        }
    }
    false
}

/// `API::_get_config` (+ `getConfig`).
async fn get_config(state: &AppState, uid: Option<i64>) -> Value {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    let num_feeds =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ttrss_feeds WHERE owner_uid = $1")
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
    // `_get_custom_sort_types` (no plugin sort hooks) → `[]`; `daemon_is_running`
    // is true — the updater runs in-process, always on (D1 = B; D6 = c).
    json!({
        "custom_sort_types": Value::Array(vec![]),
        "daemon_is_running": true,
        "icons_url": format!("{}/public.php", state.config.self_url_rtrim()),
        "num_feeds": num_feeds,
    })
}

// ── Counters / unread ───────────────────────────────────────────────────────

/// `API::getUnread` — global unread, or a feed/category counter.
async fn get_unread(state: &AppState, uid: Option<i64>, body: &Map<String, Value>) -> (i64, Value) {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    let feed_id = clean_str(body.get("feed_id").and_then(Value::as_str).unwrap_or(""));
    let is_cat = param_to_bool(body.get("is_cat"));

    let unread = if feed_id.is_empty() {
        // `Feeds::_get_global_unread`.
        global_unread(pool, uid).await
    } else {
        // `Feeds::_get_counters($feed_id, $is_cat, true)`.
        counters::get_counters(
            &state.db,
            uid,
            counters::FeedOrTag::Feed(php_int_cast(&feed_id)),
            is_cat,
            true,
            None,
        )
        .await
    };

    (STATUS_OK, json!({ "unread": unread }))
}

/// `Feeds::_get_global_unread`.
async fn global_unread(pool: &sqlx::PgPool, uid: i64) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(CASE WHEN unread THEN 1 ELSE 0 END), 0)
         FROM ttrss_user_entries WHERE owner_uid = $1",
    )
    .bind(uid)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

/// `API::getCounters` — `Counters::get_all()` (identical to `RPC::getAllCounters`).
async fn get_counters(state: &AppState, sess: &Sess, body: &Map<String, Value>) -> (i64, Value) {
    let rc = counters::get_all_counters(state, sess, body).await;
    let content = rc.get("counters").cloned().unwrap_or(Value::Array(vec![]));
    (STATUS_OK, content)
}

// ── Prefs ───────────────────────────────────────────────────────────────────

/// `API::getPref` — `Prefs::get($pref_name, uid, profile)` wrapped as `{value}`.
async fn get_pref(state: &AppState, sess: &Sess, body: &Map<String, Value>) -> (i64, Value) {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pref_name = clean_str(body.get("pref_name").and_then(Value::as_str).unwrap_or(""));
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let value = pref_json(&prefs::resolve(&pref_name, &rows, profile));
    (STATUS_OK, json!({ "value": value }))
}

/// Convert a resolved `Pref` to its JSON representation (matching
/// `Prefs::get`'s return type for the pref's `Config::T_*` hint).
fn pref_json(p: &prefs::Pref) -> Value {
    match p {
        prefs::Pref::Bool(b) => json!(*b),
        prefs::Pref::Int(i) => json!(*i),
        prefs::Pref::Str(s) => json!(s.to_string()),
    }
}

/// `API::getFeedIcon` — serve a cached feed favicon (raw image bytes, not a
/// JSON envelope) for the given feed ID, or the `E_NOT_FOUND` envelope. This is
/// the one API method that returns raw bytes: the favicon is fetched and cached
/// by the feed engine (M6), so it's absent in the test environment.
pub async fn get_feed_icon(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
    seq: i64,
) -> Response {
    // The `before()` gate still applies (auth).
    if let Err((status, content)) = api_auth(state, sess, "getfeedicon").await {
        return envelope(seq, status, content).into_response();
    }

    let id = clean_int(body.get("id"));
    let path = state.config.feed_icon_dir.join(id.to_string());
    match tokio::fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() => {
            let len = bytes.len();
            let mime = detect_image_mime(&bytes);
            let mut res = axum::http::Response::new(Body::from(bytes));
            *res.status_mut() = StatusCode::OK;
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static(mime),
            );
            res.headers_mut()
                .insert(header::CONTENT_LENGTH, axum::http::HeaderValue::from(len));
            res.into_response()
        }
        _ => envelope(seq, STATUS_ERR, json!({ "error": E_NOT_FOUND })).into_response(),
    }
}

/// Detect an image MIME type from magic bytes (the feed-icons cache stores
/// favicons by feed ID with no extension, so the type comes from the content —
/// an approximation of `DiskCache::get_mime_type`'s `finfo`).
fn detect_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 4 && &bytes[0..4] == b"\x89PNG" {
        "image/png"
    } else if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        "image/jpeg"
    } else if bytes.len() >= 4 && &bytes[0..4] == b"GIF8" {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/png"
    }
}

// ── Stubs (implemented in subsequent increments) ────────────────────────────

/// `API::getFeeds` / `API::_api_get_feeds` — the client's feed-list source.
/// Returns a *mixed* flat list, in PHP's block order: labels, special/virtual
/// feeds, child categories (when `include_nested`), then real feeds — each
/// block gated by `unread_only`. The exact per-entry shapes are what the
/// Android client parses.
async fn get_feeds(state: &AppState, uid: Option<i64>, body: &Map<String, Value>) -> (i64, Value) {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    let cat_id = clean_int(body.get("cat_id"));
    let unread_only = param_to_bool(body.get("unread_only"));
    let limit = clean_int(body.get("limit"));
    let offset = clean_int(body.get("offset"));
    let include_nested = param_to_bool(body.get("include_nested"));
    let icon_dir = &state.config.feed_icon_dir;

    let mut feeds: Vec<Value> = Vec::new();

    // Labels — API-only `cat_id` values: All (-4) or Labels (-2).
    if cat_id == CAT_ALL || cat_id == CAT_LABELS {
        for cv in counters::get_labels(&state.db, uid, None).await {
            let unread = cv["counter"].as_i64().unwrap_or(0);
            if unread != 0 || !unread_only {
                feeds.push(json!({
                    "id": cv["id"].clone(),
                    "title": cv["description"].clone(),
                    "unread": unread,
                    "cat_id": CAT_LABELS,
                }));
            }
        }
    }

    // Special/virtual feeds — API-only All (-4) or Special (-1). The PHP
    // plugin-host virtual-feed loop is a no-op pre-M7 (no plugin feeds exist).
    if cat_id == CAT_ALL || cat_id == CAT_SPECIAL {
        for feed_id in [
            feeds::FEED_STARRED,
            feeds::FEED_PUBLISHED,
            feeds::FEED_FRESH,
            feeds::FEED_ALL,
            feeds::FEED_RECENTLY_READ,
            feeds::FEED_ARCHIVED,
        ] {
            let unread = counters::get_counters(
                &state.db,
                uid,
                counters::FeedOrTag::Feed(feed_id),
                false,
                true,
                None,
            )
            .await;
            if unread != 0 || !unread_only {
                let title = feeds::feed_title(pool, feed_id, uid).await;
                feeds.push(json!({
                    "id": feed_id,
                    "title": title,
                    "unread": unread,
                    "cat_id": CAT_SPECIAL,
                }));
            }
        }
    }

    // Child categories — `include_nested && cat_id != 0`. Each category's
    // unread folds in every descendant's, matching PHP's
    // `Feeds::_get_counters($id, true, true) + Feeds::_get_cat_children_unread($id)`.
    if include_nested && cat_id != 0 {
        let cats = sqlx::query(
            "SELECT id, title, order_id FROM ttrss_feed_categories \
             WHERE parent_cat = $1 AND owner_uid = $2 \
             ORDER BY order_id ASC, title ASC",
        )
        .bind(cat_id as i32)
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for c in &cats {
            let cid: i32 = c.get("id");
            let unread =
                counters::get_counters(
                    &state.db,
                    uid,
                    counters::FeedOrTag::Feed(cid as i64),
                    true,
                    true,
                    None,
                )
                .await
                    + counters::cat_children_unread(pool, uid, cid as i64).await;
            if unread != 0 || !unread_only {
                feeds.push(json!({
                    "id": cid,
                    "title": c.get::<String, _>("title"),
                    "unread": unread,
                    "is_cat": true,
                    "order_id": c.get::<i32, _>("order_id"),
                }));
            }
        }
    }

    // Real feeds. `last_updated` via `FLOOR(EXTRACT(EPOCH FROM …))` mirrors
    // PHP's `strtotime(SUBSTRING_FOR_DATE(last_updated,1,19))` (both UTC —
    // verified against the oracle); `has_icon` is the feed-icons disk-cache
    // file-existence check (`Feeds::_has_icon`). No `cat_id` filter for the
    // API-only All (-4) / All-except-virtual (-3) values.
    let no_cat_filter = cat_id == CAT_ALL_EXCEPT_VIRTUAL || cat_id == CAT_ALL;
    let mut q = String::from(
        "SELECT id, feed_url, cat_id, title, order_id, last_error, update_interval, \
                FLOOR(EXTRACT(EPOCH FROM last_updated))::bigint AS last_updated \
         FROM ttrss_feeds WHERE owner_uid = $1",
    );
    if !no_cat_filter {
        q.push_str(" AND (cat_id = $2 OR ($2 = 0 AND cat_id IS NULL))");
    }
    q.push_str(" ORDER BY order_id ASC, title ASC");
    if limit > 0 {
        q.push_str(&format!(" LIMIT {limit}"));
    }
    if offset > 0 {
        q.push_str(&format!(" OFFSET {offset}"));
    }
    let mut query = sqlx::query(&q);
    query = query.bind(uid);
    if !no_cat_filter {
        query = query.bind(cat_id as i32);
    }
    let rows = query.fetch_all(pool).await.unwrap_or_default();
    for r in &rows {
        let feed_id: i32 = r.get("id");
        let unread = counters::get_counters(
            &state.db,
            uid,
            counters::FeedOrTag::Feed(feed_id as i64),
            false,
            true,
            None,
        )
        .await;
        if unread != 0 || !unread_only {
            let has_icon = icon_dir.join(feed_id.to_string()).is_file();
            feeds.push(json!({
                "feed_url": r.get::<String, _>("feed_url"),
                "title": r.get::<String, _>("title"),
                "id": feed_id,
                "unread": unread,
                "has_icon": has_icon,
                "cat_id": r.get::<Option<i32>, _>("cat_id").map(|c| c as i64).unwrap_or(0),
                // PHP: `(int) strtotime($feed->last_updated ?? '')` → NULL last_updated
                // (never-updated feed) yields `(int) false` = 0.
                "last_updated": r.get::<Option<i64>, _>("last_updated").unwrap_or(0),
                "order_id": r.get::<i32, _>("order_id"),
                "last_error": r.get::<Option<String>, _>("last_error"),
                "update_interval": r.get::<i32, _>("update_interval") as i64,
            }));
        }
    }

    (STATUS_OK, Value::Array(feeds))
}

async fn get_categories(
    state: &AppState,
    uid: Option<i64>,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    let unread_only = param_to_bool(body.get("unread_only"));
    // `enable_nested` — the Android client's "top level categories only":
    // list just the roots and fold every descendant's unread into each root.
    let enable_nested = param_to_bool(body.get("enable_nested"));
    let include_empty = param_to_bool(body.get("include_empty"));
    let mut cats: Vec<Value> = Vec::new();
    let q = if enable_nested {
        "SELECT c.id, c.title, c.order_id,
                (SELECT COUNT(f.id) FROM ttrss_feeds f WHERE f.cat_id = c.id) AS num_feeds,
                (SELECT COUNT(c2.id) FROM ttrss_feed_categories c2 WHERE c2.parent_cat = c.id) AS num_cats
         FROM ttrss_feed_categories c WHERE c.owner_uid = $1 AND c.parent_cat IS NULL"
    } else {
        "SELECT c.id, c.title, c.order_id,
                (SELECT COUNT(f.id) FROM ttrss_feeds f WHERE f.cat_id = c.id) AS num_feeds,
                (SELECT COUNT(c2.id) FROM ttrss_feed_categories c2 WHERE c2.parent_cat = c.id) AS num_cats
         FROM ttrss_feed_categories c WHERE c.owner_uid = $1"
    };
    let rows = sqlx::query(q)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for r in &rows {
        let id: i32 = r.get("id");
        let num_feeds: i64 = r.get("num_feeds");
        let num_cats: i64 = r.get("num_cats");
        if include_empty || num_feeds > 0 || num_cats > 0 {
            let mut unread = counters::get_counters(
                &state.db,
                uid,
                counters::FeedOrTag::Feed(id as i64),
                true,
                true,
                None,
            )
            .await;
            // The base counter only covers this category's direct feeds;
            // `enable_nested` folds in every descendant's (recursive).
            if enable_nested {
                unread += counters::cat_children_unread(pool, uid, id as i64).await;
            }
            if unread != 0 || !unread_only {
                cats.push(json!({
                    "id": id,
                    "title": r.get::<String, _>("title"),
                    "unread": unread,
                    "order_id": r.get::<i32, _>("order_id"),
                }));
            }
        }
    }
    for cat_id in [CAT_LABELS, CAT_SPECIAL, CAT_UNCATEGORIZED] {
        if include_empty || !cat_empty(pool, uid, cat_id).await {
            let unread = counters::get_counters(
                &state.db,
                uid,
                counters::FeedOrTag::Feed(cat_id),
                true,
                true,
                None,
            )
            .await;
            if unread != 0 || !unread_only {
                cats.push(json!({
                    "id": cat_id,
                    "title": cat_title(cat_id),
                    "unread": unread,
                }));
            }
        }
    }
    (STATUS_OK, Value::Array(cats))
}

fn cat_title(cat_id: i64) -> &'static str {
    match cat_id {
        CAT_UNCATEGORIZED => "Uncategorized",
        CAT_SPECIAL => "Special",
        CAT_LABELS => "Labels",
        _ => "UNKNOWN",
    }
}

/// `API::_is_cat_empty` — the PHP only ever answers `true` for LABELS (no
/// labels owned) and UNCATEGORIZED (no uncategorized feeds); every other id
/// (incl. SPECIAL) is "never empty".
async fn cat_empty(pool: &sqlx::PgPool, uid: i64, cat_id: i64) -> bool {
    if cat_id == CAT_LABELS {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(id) FROM ttrss_labels2 WHERE owner_uid = $1")
            .bind(uid)
            .fetch_one(pool)
            .await
            .unwrap_or(0)
            == 0
    } else if cat_id == CAT_UNCATEGORIZED {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(id) FROM ttrss_feeds WHERE cat_id IS NULL AND owner_uid = $1",
        )
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
        == 0
    } else {
        false
    }
}

async fn get_feed_tree(state: &AppState, sess: &Sess, body: &Map<String, Value>) -> (i64, Value) {
    let include_empty = param_to_bool(body.get("include_empty"));
    // `Pref_Feeds::_makefeedtree` with `mode = 2` + `force_show_empty = include_empty`.
    let mut extra = Map::new();
    extra.insert("mode".into(), json!("2"));
    extra.insert("force_show_empty".into(), json!(include_empty));
    let tree = feeds::getfeedtree(state, sess, &extra).await;
    (STATUS_OK, json!({ "categories": tree }))
}

async fn get_labels(state: &AppState, uid: Option<i64>, body: &Map<String, Value>) -> (i64, Value) {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    // `Article::_get_labels` for the requested article (none in the corpus → all
    // `checked` false).
    let article_id = clean_int(body.get("article_id"));
    let article_labels: Vec<Value> = if article_id > 0 {
        article::get_labels(pool, uid, article_id as i32, None).await
    } else {
        Vec::new()
    };
    let rv = labels::get_all(pool, uid)
        .await
        .iter()
        .map(|l| {
            let lid = l.get("id").and_then(Value::as_i64).unwrap_or(0);
            let checked = article_labels.iter().any(|al| {
                al.get(0)
                    .and_then(Value::as_i64)
                    .map(|f| labels::feed_to_label_id(f) == lid)
                    .unwrap_or(false)
            });
            Map::from_iter([
                ("id".into(), json!(labels::label_to_feed_id(lid))),
                (
                    "caption".into(),
                    l.get("caption").cloned().unwrap_or_default(),
                ),
                (
                    "fg_color".into(),
                    l.get("fg_color").cloned().unwrap_or_default(),
                ),
                (
                    "bg_color".into(),
                    l.get("bg_color").cloned().unwrap_or_default(),
                ),
                ("checked".into(), json!(checked)),
            ])
        })
        .collect::<Vec<_>>();
    (
        STATUS_OK,
        Value::Array(rv.into_iter().map(Value::Object).collect()),
    )
}

async fn get_headlines(state: &AppState, sess: &Sess, body: &Map<String, Value>) -> (i64, Value) {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let feed_id = match body.get("feed_id") {
        Some(Value::String(s)) => clean_str(s),
        Some(other) => clean_str(&other.to_string()),
        None => String::new(),
    };
    if feed_id.is_empty() && !feed_id.parse::<i64>().is_ok() {
        return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
    }
    let limit = clean_int(body.get("limit"));
    let limit = if limit == 0 || limit >= 200 {
        200
    } else {
        limit
    };
    let offset = clean_int(body.get("skip"));
    let is_cat = param_to_bool(body.get("is_cat"));
    let show_excerpt = param_to_bool(body.get("show_excerpt"));
    let show_content = param_to_bool(body.get("show_content"));
    let view_mode = body
        .get("view_mode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let include_attachments = param_to_bool(body.get("include_attachments"));
    let since_id = clean_int(body.get("since_id"));
    let include_nested = param_to_bool(body.get("include_nested"));
    let sanitize_content = param_bool_default(body, "sanitize", true);
    let excerpt_length = clean_int(body.get("excerpt_length"));
    // `API::getHeadlines`: per-request capability flag; PHP stores it in the
    // session (`$_SESSION['hasSandbox']`) for the sanitizer (and for later
    // `getArticle` calls in the same session).
    let has_sandbox = param_to_bool(body.get("has_sandbox"));
    sess.with_mut(|d| d.set("hasSandbox", PValue::Bool(has_sandbox)));
    let include_header = param_to_bool(body.get("include_header"));

    let (override_order, skip_first_id_check) = headlines::order_to_override_query(&clean_str(
        body.get("order_by").and_then(Value::as_str).unwrap_or(""),
    ));

    let params = headlines::HeadlinesParams {
        feed: feed_id.clone(),
        limit,
        view_mode,
        cat_view: is_cat,
        offset,
        since_id,
        owner_uid: uid,
        override_order,
        include_children: include_nested,
        skip_first_id_check,
        ..Default::default()
    };
    let q = headlines::get_headlines_query(&state.db, &params).await;

    // M7.6: the HOOK_SANITIZE context (no_iframes et al.) — built once for
    // the whole request (PHP's singleton; the active set is per owner).
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;

    let mut out: Vec<Value> = Vec::new();
    for r in &q.article_rows {
        let row = api_headline_row(
            r,
            &q.feed_title,
            uid,
            is_cat,
            q.is_vfeed,
            show_excerpt,
            show_content,
            sanitize_content,
            has_sandbox,
            include_attachments,
            excerpt_length,
            state,
            &mut pctx,
        )
        .await;
        out.push(row);
    }

    let header = json!({ "id": feed_id, "first_id": q.first_id, "is_cat": is_cat });
    if include_header {
        (STATUS_OK, json!([header, out]))
    } else {
        (STATUS_OK, Value::Array(out))
    }
}

// Non-vfeed column layout (see `headlines.rs` SELECT).
const H_ID: usize = 0;
const H_GUID: usize = 3;
const H_TITLE: usize = 4;
const H_UPDATED: usize = 5;
const H_LABEL_CACHE: usize = 6;
const H_TAG_CACHE: usize = 7;
const H_ALWAYS_ENC: usize = 8;
const H_SITE_URL: usize = 9;
const H_NOTE: usize = 10;
const H_NUM_COMMENTS: usize = 11;
const H_COMMENTS: usize = 12;
const H_LANG: usize = 15;
const H_HIDE_IMAGES: usize = 16;
const H_UNREAD: usize = 17;
const H_FEED_ID: usize = 18;
const H_MARKED: usize = 19;
const H_PUBLISHED: usize = 20;
const H_LINK: usize = 21;
const H_LAST_READ: usize = 22;
const H_CONTENT_NONVF: usize = 25;
const H_AUTHOR_NONVF: usize = 26;
const H_SCORE_NONVF: usize = 27;
const H_FEED_TITLE_VF: usize = 25;
const H_CONTENT_VF: usize = 27;
const H_AUTHOR_VF: usize = 28;
const H_SCORE_VF: usize = 29;

#[allow(clippy::too_many_arguments)] // mirrors `API::_api_get_headlines`'s format options
async fn api_headline_row(
    r: &sqlx::postgres::PgRow,
    feed_title: &str,
    uid: i64,
    is_cat: bool,
    is_vfeed: bool,
    show_excerpt: bool,
    show_content: bool,
    sanitize_content: bool,
    has_sandbox: bool,
    include_attachments: bool,
    excerpt_length: i64,
    state: &AppState,
    pctx: &mut crate::pluginhost::host::HostCtx<'_>,
) -> Value {
    let pool = state.db.pool();
    let id: i32 = r.get(H_ID);
    let has_line_feed_title = is_cat || is_vfeed;
    let (content_idx, author_idx, score_idx) = if has_line_feed_title {
        (H_CONTENT_VF, H_AUTHOR_VF, H_SCORE_VF)
    } else {
        (H_CONTENT_NONVF, H_AUTHOR_NONVF, H_SCORE_NONVF)
    };
    let title: String = r.get(H_TITLE);
    let link: String = r.get(H_LINK);
    let updated: Option<chrono::NaiveDateTime> = r.get(H_UPDATED);
    let label_cache: String = r.get(H_LABEL_CACHE);
    let tag_cache: String = r.get(H_TAG_CACHE);
    let site_url: String = r.get(H_SITE_URL);
    let note: Option<String> = r.get(H_NOTE);
    let lang: String = r.get(H_LANG);
    let hide_images: Option<bool> = r.get(H_HIDE_IMAGES);
    let unread: bool = r.get(H_UNREAD);
    let line_feed_id: i32 = r.get(H_FEED_ID);
    let marked: bool = r.get(H_MARKED);
    let published: bool = r.get(H_PUBLISHED);
    let last_read: Option<chrono::NaiveDateTime> = r.get(H_LAST_READ);
    let content: String = r.get(content_idx);
    let author: String = r.get(author_idx);
    let score: i32 = r.get(score_idx);

    let is_updated = last_read.is_none() && !unread;
    let tags: Vec<Value> = if tag_cache.is_empty() {
        Vec::new()
    } else {
        tag_cache
            .split(',')
            .map(|s| Value::from(s.to_string()))
            .collect()
    };

    let mut m = serde_json::Map::new();
    m.insert("id".into(), json!(id));
    m.insert("guid".into(), Value::from(r.get::<String, _>(H_GUID)));
    m.insert("unread".into(), json!(unread));
    m.insert("marked".into(), json!(marked));
    m.insert("published".into(), json!(published));
    m.insert(
        "updated".into(),
        json!(updated.map(|d| d.and_utc().timestamp()).unwrap_or(0)),
    );
    m.insert("is_updated".into(), json!(is_updated));
    m.insert("title".into(), Value::from(title));
    m.insert("link".into(), Value::from(link));
    m.insert(
        "feed_id".into(),
        json!(if line_feed_id == 0 { 0 } else { line_feed_id }),
    );
    m.insert("tags".into(), Value::Array(tags));

    // PHP always fetches enclosures for the flavor image; it emits them only
    // when `include_attachments` is set.
    let enclosures = if show_content || include_attachments {
        article::get_enclosures(pool, id).await
    } else {
        Vec::new()
    };
    if include_attachments {
        m.insert(
            "attachments".into(),
            Value::Array(enclosures.iter().cloned().map(Value::Object).collect()),
        );
    }
    if show_excerpt {
        let excerpt = truncate_string(&strip_tags(&content), excerpt_length as usize, "&hellip;");
        m.insert("excerpt".into(), Value::from(excerpt));
    }
    if show_content {
        let site_opt = site_url.as_str();
        let content_out = if sanitize_content {
            sanitizer::sanitize(
                &content,
                hide_images.unwrap_or(false),
                false,
                has_sandbox,
                false,
                Some(site_opt).filter(|s| !s.is_empty()),
                None,
                id,
                Some(pctx),
            )
            .await
        } else {
            content.clone()
        };
        m.insert("content".into(), Value::from(content_out));

        // `Article::_get_image` (the UNSANITIZED content): the article's
        // representative image / stream / kind, used by clients for thumbnails.
        let (flavor_image, flavor_stream, flavor_kind) =
            article::get_image(&enclosures, &content, &site_url);
        m.insert("flavor_image".into(), Value::from(flavor_image));
        m.insert("flavor_stream".into(), Value::from(flavor_stream));
        if flavor_kind != 0 {
            m.insert("flavor_kind".into(), json!(flavor_kind));
        }
    }

    let labels = api_labels(&label_cache, pool, uid, id).await;
    m.insert("labels".into(), labels);

    let feed_title_out = if has_line_feed_title {
        r.get::<String, _>(H_FEED_TITLE_VF)
    } else {
        feed_title.to_string()
    };
    m.insert("feed_title".into(), Value::from(feed_title_out));

    let num_comments: i32 = r.get(H_NUM_COMMENTS);
    m.insert("comments_count".into(), json!(num_comments));
    m.insert(
        "comments_link".into(),
        Value::from(r.get::<String, _>(H_COMMENTS)),
    );
    let always_enc: Option<bool> = r.get(H_ALWAYS_ENC);
    m.insert(
        "always_display_attachments".into(),
        json!(always_enc.unwrap_or(false)),
    );
    m.insert("author".into(), Value::from(author));
    m.insert("score".into(), json!(score.to_string()));
    m.insert(
        "note".into(),
        match note {
            Some(n) if !n.is_empty() => Value::from(n),
            _ => Value::Null,
        },
    );
    m.insert("lang".into(), Value::from(lang));
    m.insert("site_url".into(), Value::from(site_url));

    // M7.16: HOOK_RENDER_ARTICLE_API — the per-article API render hook
    // (nsfw rewrites <img> tags to blurred placeholders in API responses).
    let mut article_val = Value::Object(m);
    pctx.chain_hooks_callback(
        crate::pluginhost::HOOK_RENDER_ARTICLE_API,
        &mut article_val,
        |plugin, host, state| {
            Box::pin(async move {
                let params = serde_json::json!({ "article": state.clone() });
                let result = plugin.hook_render_article_api(host, &params).await;
                *state = result.clone();
                result
            })
        },
        |_r, _p| false,
    )
    .await;
    article_val
}

/// `API` label resolution: decode `label_cache` (array of `[id, caption, fg, bg]`),
/// falling back to `Article::_get_labels`; `no-labels` → `[]`.
async fn api_labels(label_cache: &str, pool: &sqlx::PgPool, uid: i64, id: i32) -> Value {
    let cache = label_cache.trim();
    if cache.is_empty() {
        // Empty cache: the hard way (`Article::_get_labels` → ttrss_user_labels2).
        return article::get_labels(pool, uid, id, None).await.into();
    }
    // Non-empty cache: decode; invalid/empty/no-labels → `[]` (PHP `_get_labels`
    // returns immediately on a non-empty cache, never re-queries the tables).
    match serde_json::from_str::<Value>(cache) {
        Ok(v)
            if !labels::php_empty_value(&v)
                && v.get("no-labels").and_then(Value::as_i64) != Some(1) =>
        {
            v
        }
        _ => Value::Array(Vec::new()),
    }
}

async fn get_article(
    state: &AppState,
    sess: &Sess,
    uid: Option<i64>,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = uid.unwrap_or(0);
    let pool = state.db.pool();
    let article_ids: Vec<i32> = body
        .get("article_id")
        .map(|v| {
            if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|x| {
                        x.as_i64()
                            .or_else(|| x.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                    })
                    .filter(|&x| x > 0)
                    .map(|x| x as i32)
                    .collect()
            } else if let Some(n) = v.as_i64() {
                if n > 0 {
                    vec![n as i32]
                } else {
                    Vec::new()
                }
            } else if let Some(s) = v.as_str() {
                s.split(',')
                    .filter_map(|p| p.trim().parse::<i32>().ok())
                    .filter(|&x| x > 0)
                    .collect()
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();

    if article_ids.is_empty() {
        return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
    }
    let sanitize_content = param_bool_default(body, "sanitize", true);

    let rows = sqlx::query(
        "SELECT e.id, e.guid, e.title, e.link, e.author, e.content, e.lang, e.comments,
                ue.feed_id, ue.marked, ue.unread, ue.published, ue.score, ue.note,
                e.updated,
                (SELECT title FROM ttrss_feeds WHERE id = ue.feed_id) AS feed_title,
                (SELECT site_url FROM ttrss_feeds WHERE id = ue.feed_id) AS site_url,
                (SELECT hide_images FROM ttrss_feeds WHERE id = ue.feed_id) AS hide_images
         FROM ttrss_entries e
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id
         WHERE e.id = ANY($1::int[]) AND ue.owner_uid = $2",
    )
    .bind(article_ids)
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let sanitize_rows = prefs::load_rows(&state.db, uid, None).await;
    let strip_images = prefs::resolve(prefs::STRIP_IMAGES, &sanitize_rows, None).as_bool();
    // `API::getArticle` sanitizes with the session's `hasSandbox` (set by the
    // client's `getHeadlines` calls) — no request param of its own.
    let has_sandbox = sess.data().get_bool("hasSandbox").unwrap_or(false);

    // M7.6: the HOOK_SANITIZE context (built once per request).
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;

    let mut out: Vec<Value> = Vec::new();
    for r in &rows {
        let id: i32 = r.get(0);
        let content: String = r.get(5);
        let hide_images: Option<bool> = r.get(17);
        let site_url: Option<String> = r.get(16);
        let updated: Option<chrono::NaiveDateTime> = r.get(14);

        let mut m = serde_json::Map::new();
        m.insert("id".into(), json!(id));
        m.insert("guid".into(), Value::from(r.get::<String, _>(1)));
        m.insert("title".into(), Value::from(r.get::<String, _>(2)));
        m.insert("link".into(), Value::from(r.get::<String, _>(3)));
        m.insert(
            "labels".into(),
            article::get_labels(pool, uid, id, None).await.into(),
        );
        let unread: bool = r.get(10);
        let marked: bool = r.get(9);
        let published: bool = r.get(11);
        m.insert("unread".into(), json!(unread));
        m.insert("marked".into(), json!(marked));
        m.insert("published".into(), json!(published));
        m.insert("comments".into(), Value::from(r.get::<String, _>(7)));
        m.insert("author".into(), Value::from(r.get::<String, _>(4)));
        m.insert(
            "updated".into(),
            json!(updated
                .map(|d| (d.and_utc().timestamp() / 60) * 60)
                .unwrap_or(0)),
        );
        let feed_id: i32 = r.get(8);
        m.insert("feed_id".into(), json!(feed_id));
        m.insert(
            "attachments".into(),
            Value::Array(
                article::get_enclosures(pool, id)
                    .await
                    .into_iter()
                    .map(Value::Object)
                    .collect(),
            ),
        );
        let score: i32 = r.get(12);
        m.insert("score".into(), json!(score.to_string()));
        m.insert(
            "feed_title".into(),
            Value::from(r.get::<Option<String>, _>(15).unwrap_or_default()),
        );
        let note: Option<String> = r.get(13);
        m.insert(
            "note".into(),
            match note {
                Some(n) if !n.is_empty() => Value::from(n),
                _ => Value::Null,
            },
        );
        m.insert("lang".into(), Value::from(r.get::<String, _>(6)));

        let content_out = if sanitize_content {
            sanitizer::sanitize(
                &content,
                hide_images.unwrap_or(false),
                strip_images,
                has_sandbox,
                false,
                site_url.as_deref().filter(|s| !s.is_empty()),
                None,
                id,
                Some(&mut pctx),
            )
            .await
        } else {
            content
        };
        m.insert("content".into(), Value::from(content_out));

        out.push(Value::Object(m));
    }
    (STATUS_OK, Value::Array(out))
}

// ── Writes (M5) ─────────────────────────────────────────────────────────────

/// `API::catchupFeed` — mark a feed / category / everything read.
pub async fn catchup_feed(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let clean_param = |k: &str| clean_str(body.get(k).and_then(|v| v.as_str()).unwrap_or(""));
    let feed_id = clean_param("feed_id");
    let is_cat = body.get("is_cat").is_some_and(|v| param_to_bool(Some(v)));
    let mode = clean_param("mode");
    // `if (!in_array($mode, ["all","1day","1week","2week"])) $mode = "all";`
    let mode = if matches!(mode.as_str(), "all" | "1day" | "1week" | "2week") {
        mode
    } else {
        "all".to_string()
    };
    let search_query = clean_param("search_query");
    let search_lang = clean_param("search_lang");
    crate::feeds::catchup_scope(
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
    (STATUS_OK, json!({ "status": "OK" }))
}

/// `API::unsubscribeFeed` — remove the session user's feed, or `E_OPERATION_FAILED`.
pub async fn unsubscribe_feed(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let feed_id = clean_int(body.get("feed_id")) as i32;
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2",
    )
    .bind(feed_id)
    .bind(uid)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    if exists > 0 {
        crate::feeds::remove_feed(state, feed_id, uid).await;
        (STATUS_OK, json!({ "status": "OK" }))
    } else {
        (STATUS_ERR, json!({ "error": E_OPERATION_FAILED }))
    }
}

/// `API::updateFeed` — update a single feed now (a blocking, in-process
/// on-demand update, D2 = a). The PHP guards on `!ini_get("open_basedir")`
/// (always unset in this port), then runs `RSSUtils::update_rss_feed`. An
/// on-demand update for a feed already in flight (claimed by the running daemon
/// loop within the last minute) is a no-op.
pub async fn update_feed(
    state: &AppState,
    _sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let pool = state.db.pool();
    let feed_id = clean_int(body.get("feed_id")) as i32;
    let in_flight: Option<bool> = sqlx::query_scalar(
        "SELECT last_update_started IS NOT NULL \
         AND last_update_started >= NOW() - INTERVAL '1 minute' \
         FROM ttrss_feeds WHERE id = $1",
    )
    .bind(feed_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if in_flight.unwrap_or(false) {
        return (STATUS_OK, json!({ "status": "OK" }));
    }
    let _ = crate::updates::fetch_one_feed(
        pool,
        &state.config,
        state.plugins.as_ref(),
        feed_id,
        &crate::updates::FeedUpdateFlags::default(),
        None,
    )
    .await;
    (STATUS_OK, json!({ "status": "OK" }))
}

/// `API::subscribeToFeed` — subscribe to a feed (delegates to
/// `Feeds::_subscribe`). Returns `{"status": <rc>}` where `<rc>` is the `{code,
/// …}` array, or `INCORRECT_USAGE` when no `feed_url` is given.
pub async fn subscribe_to_feed(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = sess.data().uid().unwrap_or(0);
    let access_level = sess.data().access_level().unwrap_or(0) as i32;
    let pool = state.db.pool();

    let feed_url = clean_str(body.get("feed_url").and_then(|v| v.as_str()).unwrap_or(""));
    let category_id = clean_int(body.get("category_id")) as i32;
    let login = clean_str(body.get("login").and_then(|v| v.as_str()).unwrap_or(""));
    let password = clean_str(body.get("password").and_then(|v| v.as_str()).unwrap_or(""));

    if feed_url.is_empty() {
        return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
    }

    // No `update_interval` param (the PHP defaults it to 0).
    let rc = crate::feeds::subscribe(
        &state.config,
        pool,
        uid,
        access_level,
        &feed_url,
        category_id,
        &login,
        &password,
        0,
    )
    .await;
    (STATUS_OK, json!({ "status": rc }))
}

/// `API::updateArticle` — batch-update a set of articles' marked / published /
/// unread / note / score. The value is a controlled SQL literal (a boolean,
/// `NOT <field>`, a `quote($data)` string, or an int), matching the PHP's
/// string-interpolated `SET <field> = <set_to>`.
pub async fn update_article(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let article_ids = param_int_array(body, "article_ids");
    if article_ids.is_empty() {
        return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
    }
    let mode = clean_int(body.get("mode"));
    let data = clean_str(body.get("data").and_then(Value::as_str).unwrap_or(""));
    let field_raw = clean_int(body.get("field"));

    // (field column, extra timestamp clause, value). `set_to` is falsy when the
    // mode is not 0/1/2 for marked/published/unread, or the score is 0 → PHP
    // `if ($field && $set_to)` → `E_INCORRECT_USAGE`.
    let (field, additional, val) = match field_raw {
        0 => ("marked", ",last_marked = NOW()", mode_val(mode, "marked")),
        1 => (
            "published",
            ",last_published = NOW()",
            mode_val(mode, "published"),
        ),
        2 => ("unread", ",last_read = NOW()", mode_val(mode, "unread")),
        3 => ("note", "", ArticleVal::Note),
        4 => {
            let score = clean_int(body.get("data"));
            if score == 0 {
                return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
            }
            ("score", "", ArticleVal::Score(score))
        }
        _ => return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE })),
    };
    if matches!(val, ArticleVal::Falsy) {
        return (STATUS_ERR, json!({ "error": E_INCORRECT_USAGE }));
    }
    let has_param = matches!(val, ArticleVal::Note | ArticleVal::Score(_));

    let k = article_ids.len();
    let qmarks: Vec<String> = (1..=k).map(|i| format!("${i}")).collect();
    let uid_param = if has_param { k + 2 } else { k + 1 };
    let value_sql = match &val {
        ArticleVal::Lit(lit) => lit.clone(),
        ArticleVal::Note | ArticleVal::Score(_) => format!("${}", k + 1),
        ArticleVal::Falsy => unreachable!("checked above"),
    };
    let sql = format!(
        "UPDATE ttrss_user_entries SET {field} = {value_sql}{additional} \
         WHERE ref_id IN ({}) AND owner_uid = ${uid_param}",
        qmarks.join(", ")
    );

    let mut q = sqlx::query(&sql);
    for id in &article_ids {
        q = q.bind(*id);
    }
    if let ArticleVal::Note = val {
        q = q.bind(&data);
    } else if let ArticleVal::Score(s) = val {
        q = q.bind(s);
    }
    q = q.bind(uid);

    // NOTE: the `marked`/`published` plugin hooks (HOOK_ARTICLES_*_TOGGLED) are
    // no-ops without plugins (M7).
    let num_updated = q
        .execute(pool)
        .await
        .map(|r| r.rows_affected() as i64)
        .unwrap_or(0);
    (STATUS_OK, json!({ "status": "OK", "updated": num_updated }))
}

/// The `marked`/`published`/`unread` value for a mode: `false`/`true`/
/// `NOT <field>`, or `Falsy` (PHP `$set_to = null`) for other modes.
fn mode_val(mode: i64, field: &str) -> ArticleVal {
    match mode {
        0 => ArticleVal::Lit("false".to_string()),
        1 => ArticleVal::Lit("true".to_string()),
        2 => ArticleVal::Lit(format!("NOT {field}")),
        _ => ArticleVal::Falsy,
    }
}

/// The SQL value for an `updateArticle` field (a controlled literal or param).
enum ArticleVal {
    /// A SQL literal fragment (`false` / `true` / `NOT <field>`).
    Lit(String),
    /// The `note` column: a text param (PHP `quote($data)` — always truthy).
    Note,
    /// The `score` column: an int param (truthy iff != 0).
    Score(i64),
    /// PHP `$set_to = null` → falsy → `E_INCORRECT_USAGE`.
    Falsy,
}

/// `API::setArticleLabel` — assign or remove a label from a set of articles.
pub async fn set_article_label(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let article_ids = param_int_array(body, "article_ids");
    let label_id = clean_int(body.get("label_id")) as i32;
    let assign = param_to_bool(body.get("assign"));
    let label = crate::labels::find_caption(
        pool,
        crate::labels::feed_to_label_id(label_id as i64) as i32,
        uid,
    )
    .await;
    let mut num_updated = 0i64;
    if !label.is_empty() {
        for &id in &article_ids {
            if assign {
                crate::labels::add_article(pool, id, &label, uid).await;
            } else {
                crate::labels::remove_article(pool, id, &label, uid).await;
            }
            num_updated += 1;
        }
    }
    (STATUS_OK, json!({ "status": "OK", "updated": num_updated }))
}

/// `API::shareToPublished` — create or update a published article for the
/// given title/url/content and mark it published for the current user.
pub async fn share_to_published(
    state: &AppState,
    sess: &Sess,
    body: &Map<String, Value>,
) -> (i64, Value) {
    let uid = sess.data().uid().unwrap_or(0);
    let title = clean_str(body.get("title").and_then(Value::as_str).unwrap_or(""));
    let url = clean_str(body.get("url").and_then(Value::as_str).unwrap_or(""));
    let sanitize = param_bool_default(body, "sanitize", true);
    let content_raw = body.get("content").and_then(Value::as_str).unwrap_or("");
    let content = if sanitize {
        clean_str(content_raw)
    } else {
        content_raw.to_string()
    };
    let ok =
        crate::article::create_published_article(state.db.pool(), &title, &url, &content, "", uid)
            .await;
    if ok {
        (STATUS_OK, json!({ "status": "OK" }))
    } else {
        (STATUS_ERR, json!({ "error": E_OPERATION_FAILED }))
    }
}
