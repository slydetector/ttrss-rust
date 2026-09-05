//! Port of the `backend.php` entry point + the `RPC` handler (M2 read ops).
//!
//! `backend.php` is the internal JSON RPC the Dojo SPA drives:
//! `POST|GET op=<Handler>&method=<m>&csrf_token=…`. This module ports the
//! dispatch (public shim, session validation, CSRF, handler routing) and the
//! `RPC` handler's `sanityCheck` (the SPA's first call, which un-blocks the
//! load sequence) and `getRuntimeInfo`. Other handlers/methods return
//! `E_UNKNOWN_METHOD` until ported.

use axum::body::Body;
use axum::extract::{Form, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{Datelike, Timelike};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::user;

use crate::feeds;
use crate::prefs;
use crate::session::{self, Sess};
use crate::AppState;

/// `ttrss_users` access level: disabled (mirrors `UserHelper::ACCESS_LEVEL_*`).
const ACCESS_LEVEL_DISABLED: i32 = -2;

/// Form/query params for `backend.php` (all arrive via `$_REQUEST`).
#[derive(Deserialize)]
pub struct RpcForm {
    op: Option<String>,
    method: Option<String>,
    subop: Option<String>,
    csrf_token: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// `backend.php` — dispatch on `op`/`method`, return a JSON envelope.
pub async fn handler(
    State(state): State<AppState>,
    sess: axum::extract::Extension<Sess>,
    form: Form<RpcForm>,
) -> Response {
    let op = form.op.clone().unwrap_or_default();
    let method = form
        .subop
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| form.method.clone())
        .unwrap_or_else(|| "index".into());
    let csrf = form.csrf_token.clone().unwrap_or_default();
    let extra = form.extra.clone();

    // `backend.php` public-calls compatibility shim.
    if matches!(op.as_str(), "rss" | "getProfiles" | "share") {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("public.php?{}", state.config.self_url_rtrim()),
            )],
        )
            .into_response();
    }

    let body = dispatch(&state, &sess, &op, &method, &csrf, &extra).await;

    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Body::from(body.to_string()))
        .expect("valid response");
    // The PHP session `nocache` limiter stamps responses whose session started
    // (i.e. the request carried a session cookie).
    if sess.had_cookie() {
        super::nocache_headers(res.headers_mut());
    }
    res
}

async fn dispatch(
    state: &AppState,
    sess: &Sess,
    op: &str,
    method: &str,
    csrf: &str,
    extra: &Map<String, Value>,
) -> Value {
    let data = sess.data();

    // `Sessions::validate_session` (light port): a logged-in session is valid
    // while the user still exists, is not disabled, and the pwd hash matches.
    if let Some(uid) = data.uid() {
        let user = sqlx::query("SELECT access_level, pwd_hash FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(state.db.pool())
            .await
            .ok()
            .flatten();
        let ok = match user {
            Some(r) => {
                let access_level = r.get::<i32, _>(0);
                let pwd_hash = r.get::<String, _>(1);
                access_level != ACCESS_LEVEL_DISABLED && data.pwd_hash() == Some(pwd_hash.as_str())
            }
            None => false,
        };
        if !ok {
            return errors("E_UNAUTHORIZED");
        }
    }

    if method.starts_with('_') {
        return errors("E_UNAUTHORIZED");
    }

    // CSRF gate: `validate_csrf($csrf_token) || $handler->csrf_ignore($method)`.
    if !validate_csrf(sess, csrf) && !csrf_ignored(op, method) {
        return errors("E_UNAUTHORIZED");
    }

    // `Handler_Protected::before`: the handler needs a logged-in user.
    if is_protected(op) && data.uid().is_none() {
        return errors("E_UNAUTHORIZED");
    }

    match op {
        "RPC" => rpc_handler(state, sess, method, extra).await,
        "Feeds" => match method {
            "view" => crate::headlines::view(state, sess, extra).await,
            "search" => crate::headlines::search(state, sess, extra).await,
            _ => unknown_method("Feeds", method),
        },
        "Pref_Feeds" => match method {
            "getfeedtree" => feeds::getfeedtree(state, sess, extra).await,
            "feedsWithErrors" => feeds::feeds_with_errors(state, sess).await,
            _ => unknown_method("Pref_Feeds", method),
        },
        other => unknown_method(other, method),
    }
}

/// `Errors::to_json(code, params)`.
fn errors(code: &str) -> Value {
    json!({ "error": { "code": code, "params": [] } })
}

/// `Errors::to_json(E_UNKNOWN_METHOD, ["info" => "<Handler>-><method>"])`.
fn unknown_method(handler: &str, method: &str) -> Value {
    json!({ "error": { "code": "E_UNKNOWN_METHOD", "params": [ { "info": format!("{handler}->{method}") } ] } })
}

/// The `RPC` handler (`Handler_Protected`): CSRF-gated (in [`dispatch`]), no
/// per-method `csrf_ignore`.
async fn rpc_handler(
    state: &AppState,
    sess: &Sess,
    method: &str,
    extra: &Map<String, Value>,
) -> Value {
    match method {
        "sanityCheck" => sanity_check(state, sess, extra).await,
        "getRuntimeInfo" => json!({ "runtime-info": make_runtime_info(state, sess).await }),
        "getAllCounters" => crate::counters::get_all_counters(state, sess, extra).await,
        // M3 mutations (article state).
        "catchupSelected" => crate::article::catchup_selected(state, sess, extra).await,
        "markSelected" => crate::article::mark_selected(state, sess, extra).await,
        "publishSelected" => crate::article::publish_selected(state, sess, extra).await,
        other => unknown_method("RPC", other),
    }
}

/// `validate_csrf`: the request token matches the session token (non-empty).
fn validate_csrf(sess: &Sess, csrf: &str) -> bool {
    let data = sess.data();
    let session_csrf = data.csrf_token().unwrap_or("");
    !csrf.is_empty() && ct_eq(session_csrf, csrf)
}

/// `Handler::csrf_ignore` per handler. Only the read-only handlers the SPA
/// drives via GET (no token) are exempt.
fn csrf_ignored(op: &str, method: &str) -> bool {
    // `Pref_Feeds::csrf_ignore` = ["index", "getfeedtree"].
    matches!(op, "Pref_Feeds") && matches!(method, "index" | "getfeedtree")
        // `Feeds::csrf_ignore` = ["index"].
        || (op == "Feeds" && method == "index")
}

/// `Handler_Protected` handlers (all the ones ported so far) require a user.
fn is_protected(op: &str) -> bool {
    matches!(op, "RPC" | "Feeds" | "Pref_Feeds")
}

/// `RPC::sanityCheck`.
async fn sanity_check(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);

    // Stored (not returned) — mirrors `$_SESSION["hasSandbox"/"clientTzOffset"]`.
    sess.with_mut(|s| {
        if let Some(v) = extra.get("hasSandbox").and_then(Value::as_str) {
            s.set("hasSandbox", ttrss_core::phpser::Value::Bool(
                !v.is_empty() && v != "f" && v != "false",
            ));
        }
        if let Some(v) = extra.get("clientTzOffset").and_then(Value::as_str) {
            s.set("clientTzOffset", ttrss_core::phpser::Value::Bytes(v.trim().as_bytes().to_vec()));
        }
    });

    // `$_REQUEST["clientLocation"]` (the request, not the session).
    let client_location = extra
        .get("clientLocation")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let client_scheme = url_scheme(&client_location);
    let server_scheme = url_scheme(&state.config.self_url_rtrim());

    let mut err = "E_SUCCESS";
    let mut params: Map<String, Value> = Map::new();
    if client_scheme != server_scheme {
        err = "E_URL_SCHEME_MISMATCH";
        params.insert(
            "client_scheme".into(),
            client_scheme.map(Value::from).unwrap_or(Value::Null),
        );
        params.insert(
            "server_scheme".into(),
            Value::from(server_scheme.clone().unwrap_or_default()),
        );
        params.insert("self_url_path".into(), Value::from(state.config.self_url_rtrim()));
    }

    if err == "E_SUCCESS" {
        json!({
            "init-params": make_init_params(state, sess, uid, profile).await,
            "runtime-info": make_runtime_info(state, sess).await,
            "translations": Value::Array(vec![]),
        })
    } else {
        json!({ "error": { "code": err, "params": params } })
    }
}

/// `RPC::_make_runtime_info`.
pub(crate) async fn make_runtime_info(state: &AppState, sess: &Sess) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let access_level = data.access_level().unwrap_or(0);

    let (max_feed_id, num_feeds) = feed_counts(state.db.pool(), uid).await;
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let cdm_expanded = prefs::resolve("CDM_EXPANDED", &rows, profile).as_bool();
    let labels = labels_all(state.db.pool(), uid).await;

    let mut out = Map::new();
    out.insert("max_feed_id".into(), Value::from(max_feed_id));
    out.insert("num_feeds".into(), Value::from(num_feeds));
    out.insert("cdm_expanded".into(), Value::from(cdm_expanded));
    out.insert("labels".into(), labels);

    // `recent_log_events` (admin + `LOG_DESTINATION == sql`) is a live counter;
    // the harness normalizes it away, so we emit it for shape parity.
    if access_level >= 10 {
        let cid = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(id) FROM ttrss_error_log
             WHERE errno NOT IN (512, 8192)
               AND created_at > NOW() - INTERVAL '1 hour'
               AND errstr NOT LIKE '%Returning bool from comparison function is deprecated%'
               AND errstr NOT LIKE '%imagecreatefromstring(): Data is not in a recognized format%'",
        )
        .fetch_one(state.db.pool())
        .await
        .unwrap_or(0);
        out.insert("recent_log_events".into(), Value::from(cid));
    }

    // Update-daemon state (LOCK_DIRECTORY defaults to `lock` under the doc
    // root). Absent on the daemon-less Rust test server; the harness drops
    // these volatile keys anyway.
    let lock_dir = state.config.docroot.join("lock");
    if lock_dir.join("update_daemon.lock").exists() {
        out.insert(
            "daemon_is_running".into(),
            Value::from(file_is_locked(&lock_dir.join("update_daemon.lock")) as i64),
        );
        let now = chrono::Utc::now().timestamp();
        let stamp_check = data.get_int("daemon_stamp_check").unwrap_or(0);
        if now - stamp_check > 30 {
            let stamp = std::fs::read_to_string(lock_dir.join("update_daemon.stamp"))
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(0);
            if stamp != 0 {
                let delta = now - stamp;
                let stamp_ok = if delta > 1800 { 0 } else { 1 };
                if stamp_ok == 1 {
                    sess.with_mut(|s| {
                        s.set("daemon_stamp_check", ttrss_core::phpser::Value::Int(now));
                    });
                }
                out.insert("daemon_stamp_ok".into(), Value::from(stamp_ok));
                // `date("Y.m.d, G:i", $stamp)` (G = 24h clock, no leading zero)
                out.insert(
                    "daemon_stamp".into(),
                    Value::from(
                        chrono::DateTime::from_timestamp(stamp, 0)
                            .map(|dt| {
                                format!(
                                    "{:04}.{:02}.{:02}, {}:{:02}",
                                    dt.year(),
                                    dt.month(),
                                    dt.day(),
                                    dt.hour(),
                                    dt.minute()
                                )
                            })
                            .unwrap_or_default(),
                    ),
                );
            }
        }
    }
    Value::Object(out)
}

/// PHP `file_is_locked`: is another process holding an exclusive `flock` on
/// the file? A non-blocking exclusive lock attempt succeeds iff it's free.
#[cfg(unix)]
fn file_is_locked(path: &std::path::Path) -> bool {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    match std::fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) == -1 },
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn file_is_locked(_path: &std::path::Path) -> bool {
    false
}

/// `RPC::_make_init_params`.
async fn make_init_params(
    state: &AppState,
    sess: &Sess,
    uid: i64,
    profile: Option<i32>,
) -> Value {
    let data = sess.data();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let p = |name: &str| prefs::resolve(name, &rows, profile);

    let (max_feed_id, num_feeds) = feed_counts(state.db.pool(), uid).await;
    let labels = labels_all(state.db.pool(), uid).await;
    let is_default_pw = is_default_password(state.db.pool(), uid).await;
    let self_url = state.config.self_url_rtrim();

    let theme_pref = p("USER_CSS_THEME").as_str().to_string();
    let theme = if theme_exists(&state.config.docroot, &theme_pref) {
        theme_pref
    } else {
        String::new()
    };

    let (prefixes, hotkeys) = hotkeys_map();

    let mut out = Map::new();
    out.insert("on_catchup_show_next_feed".into(), Value::from(p("ON_CATCHUP_SHOW_NEXT_FEED").as_int()));
    out.insert("hide_read_feeds".into(), Value::from(p("HIDE_READ_FEEDS").as_int()));
    out.insert("enable_feed_cats".into(), Value::from(p("ENABLE_FEED_CATS").as_int()));
    out.insert("feeds_sort_by_unread".into(), Value::from(p("FEEDS_SORT_BY_UNREAD").as_int()));
    out.insert("confirm_feed_catchup".into(), Value::from(p("CONFIRM_FEED_CATCHUP").as_int()));
    out.insert("cdm_auto_catchup".into(), Value::from(p("CDM_AUTO_CATCHUP").as_int()));
    out.insert("fresh_article_max_age".into(), Value::from(p("FRESH_ARTICLE_MAX_AGE").as_int()));
    out.insert("recently_read_max_age".into(), Value::from(p("RECENTLY_READ_MAX_AGE").as_int()));
    out.insert("hide_read_shows_special".into(), Value::from(p("HIDE_READ_SHOWS_SPECIAL").as_int()));
    out.insert("combined_display_mode".into(), Value::from(p("COMBINED_DISPLAY_MODE").as_int()));
    out.insert("debug_headline_ids".into(), Value::from(p("DEBUG_HEADLINE_IDS").as_int()));
    out.insert("cdm_enable_grid".into(), Value::from(p("CDM_ENABLE_GRID").as_int()));
    out.insert("safe_mode".into(), Value::from(data.safe_mode()));
    out.insert("check_for_updates".into(), Value::from(check_for_updates()));
    out.insert("icons_url".into(), Value::from(format!("{self_url}/public.php")));
    out.insert("cookie_lifetime".into(), Value::from(session::COOKIE_LIFETIME));
    out.insert("default_view_mode".into(), Value::from(p("_DEFAULT_VIEW_MODE").as_str()));
    out.insert("default_view_limit".into(), Value::from(p("_DEFAULT_VIEW_LIMIT").as_int()));
    out.insert("default_view_order_by".into(), Value::from(p("_DEFAULT_VIEW_ORDER_BY").as_str()));
    out.insert("bw_limit".into(), Value::from(data.get_bool("bw_limit").unwrap_or(false) as i64));
    out.insert("is_default_pw".into(), Value::from(is_default_pw));
    out.insert("label_base_index".into(), Value::from(-1024i64));
    out.insert("theme".into(), Value::from(theme));
    out.insert("plugins".into(), Value::from(plugin_names(&state.config.docroot)));
    out.insert("php_platform".into(), Value::from("Linux"));
    out.insert("php_version".into(), Value::from("8.5.9"));
    out.insert("self_url_prefix".into(), Value::from(self_url));
    out.insert("max_feed_id".into(), Value::from(max_feed_id));
    out.insert("num_feeds".into(), Value::from(num_feeds));
    out.insert("hotkeys".into(), json!([prefixes, hotkeys]));
    out.insert("widescreen".into(), Value::from(p("WIDESCREEN_MODE").as_int()));
    out.insert(
        "icon_indicator_white".into(),
        image_base64(&state.config.docroot, "images/indicator_white.gif"),
    );
    out.insert("icon_oval".into(), image_base64(&state.config.docroot, "images/oval.svg"));
    out.insert(
        "icon_three_dots".into(),
        image_base64(&state.config.docroot, "images/three-dots.svg"),
    );
    out.insert("icon_blank".into(), image_base64(&state.config.docroot, "images/blank_icon.gif"));
    out.insert("labels".into(), labels);
    Value::Object(out)
}

/// `Labels::get_all` — `{id, fg_color, bg_color, caption}` ordered by caption.
async fn labels_all(pool: &sqlx::PgPool, uid: i64) -> Value {
    let rows = sqlx::query(
        "SELECT id, fg_color, bg_color, caption FROM ttrss_labels2
         WHERE owner_uid = $1 ORDER BY caption",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "id": r.get::<i32, _>(0),
                    "fg_color": r.get::<String, _>(1),
                    "bg_color": r.get::<String, _>(2),
                    "caption": r.get::<String, _>(3),
                })
            })
            .collect(),
    )
}

/// `SELECT MAX(id), COUNT(*) FROM ttrss_feeds WHERE owner_uid = ?`.
async fn feed_counts(pool: &sqlx::PgPool, uid: i64) -> (i64, i64) {
    let row = sqlx::query("SELECT MAX(id), COUNT(*) FROM ttrss_feeds WHERE owner_uid = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => (r.get::<Option<i32>, _>(0).unwrap_or(0) as i64, r.get::<i64, _>(1)),
        None => (0, 0),
    }
}

/// `UserHelper::is_default_password` — does the user's password equal the
/// literal string "password"?
async fn is_default_password(pool: &sqlx::PgPool, uid: i64) -> bool {
    let row = sqlx::query("SELECT pwd_hash, salt FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(r) = row else {
        return false;
    };
    user::verify_password(&r.get::<String, _>(0), "password", &r.get::<String, _>(1))
}

/// `theme_exists`: a bare `themes/<name>` file/dir (or a local-themes dir).
fn theme_exists(docroot: &std::path::Path, theme: &str) -> bool {
    if theme.is_empty() {
        return false;
    }
    docroot.join("themes").join(theme).exists()
        || docroot.join("themes.local").join(theme).exists()
}

/// `RPC::image_to_base64` — `data:image/<ext>;base64,<b64>` ("" if missing).
fn image_base64(docroot: &std::path::Path, filename: &str) -> Value {
    let path = docroot.join(filename);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let ext = std::path::Path::new(filename)
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            let mime_ext = if ext == "svg" { "svg+xml" } else { &ext };
            Value::from(format!(
                "data:image/{mime_ext};base64,{}",
                base64_encode(&bytes)
            ))
        }
        Err(_) => Value::from(String::new()),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `TTRSS_CHECK_FOR_UPDATES` (default `true`).
fn check_for_updates() -> bool {
    std::env::var("TTRSS_CHECK_FOR_UPDATES")
        .map(|v| v != "false" && v != "0" && !v.is_empty())
        .unwrap_or(true)
}

/// `PluginHost::get_plugin_names` for the web session: the `TTRSS_PLUGINS`
/// init list (KIND_ALL) — the same list is loaded for every request, so the
/// per-user enabled subset only re-registers (order is fixed by the init list).
fn plugin_names(docroot: &std::path::Path) -> String {
    let list = std::env::var("TTRSS_PLUGINS").unwrap_or_else(|_| "auth_internal".into());
    list.split(',')
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .filter_map(|d| plugin_class_name(docroot, d))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve a plugin dir name to its PHP class name by reading the plugin's
/// `init.php` (`plugins/<dir>` first, then `plugins.local/<dir>`).
fn plugin_class_name(docroot: &std::path::Path, dir: &str) -> Option<String> {
    for base in ["plugins", "plugins.local"] {
        let init = docroot.join(base).join(dir).join("init.php");
        let Ok(src) = std::fs::read_to_string(&init) else {
            continue;
        };
        for line in src.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("class ") {
                let name = rest
                    .trim()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// `RPC::get_hotkeys_map` — `[prefixes, {sequence: action}]`.
fn hotkeys_map() -> (Vec<String>, Map<String, Value>) {
    let mut map: Map<String, Value> = Map::new();
    let entries: [(&str, &str); 61] = [
        ("k", "next_feed"),
        ("K", "next_unread_feed"),
        ("j", "prev_feed"),
        ("J", "prev_unread_feed"),
        ("n", "next_article_noscroll"),
        ("p", "prev_article_noscroll"),
        ("N", "article_page_down"),
        ("P", "article_page_up"),
        ("*(33)|Shift+PgUp", "article_page_up"),
        ("*(34)|Shift+PgDn", "article_page_down"),
        ("{3}(38)|Up", "prev_article_or_scroll"),
        ("{3}(40)|Down", "next_article_or_scroll"),
        ("*(38)|Shift+Up", "article_scroll_up"),
        ("*(40)|Shift+Down", "article_scroll_down"),
        ("^(38)|Ctrl+Up", "prev_article_noscroll"),
        ("^(40)|Ctrl+Down", "next_article_noscroll"),
        ("/", "search_dialog"),
        ("\\", "cancel_search"),
        ("s", "toggle_mark"),
        ("S", "toggle_publ"),
        ("u", "toggle_unread"),
        ("T", "edit_tags"),
        ("o", "open_in_new_window"),
        ("c p", "catchup_below"),
        ("c n", "catchup_above"),
        ("a W", "toggle_widescreen"),
        ("a e", "toggle_full_text"),
        ("e", "email_article"),
        ("a q", "close_article"),
        ("a s", "article_span_grid"),
        ("a a", "select_all"),
        ("a u", "select_unread"),
        ("a U", "select_marked"),
        ("a p", "select_published"),
        ("a i", "select_invert"),
        ("a n", "select_none"),
        ("f r", "feed_refresh"),
        ("f a", "feed_unhide_read"),
        ("f s", "feed_subscribe"),
        ("f e", "feed_edit"),
        ("f q", "feed_catchup"),
        ("f x", "feed_reverse"),
        ("f g", "feed_toggle_vgroup"),
        ("f G", "feed_toggle_grid"),
        ("f D", "feed_debug_update"),
        ("f %", "feed_debug_viewfeed"),
        ("f C", "toggle_combined_mode"),
        ("f c", "toggle_cdm_expanded"),
        ("Q", "catchup_all"),
        ("x", "cat_toggle_collapse"),
        ("g a", "goto_all"),
        ("g f", "goto_fresh"),
        ("g s", "goto_marked"),
        ("g p", "goto_published"),
        ("g r", "goto_read"),
        ("g P", "goto_prefs"),
        ("r", "select_article_cursor"),
        ("c l", "create_label"),
        ("c f", "create_filter"),
        ("c s", "collapse_sidebar"),
        ("?", "help_dialog"),
    ];
    let mut prefixes: Vec<String> = Vec::new();
    for (seq, action) in entries {
        map.insert(seq.to_string(), Value::from(action.to_string()));
        if let Some(first) = seq.split(' ').next() {
            if seq.contains(' ') && !prefixes.iter().any(|p| p == first) {
                prefixes.push(first.to_string());
            }
        }
    }
    (prefixes, map)
}

/// `hash_equals` (constant-time; false on length mismatch).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// The `scheme` of a URL (None if absent/unparseable) — `parse_url(.., SCHEME)`.
fn url_scheme(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.scheme().to_string())
}
