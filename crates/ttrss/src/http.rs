//! `ttrss` — the Rust HTTP server for Tiny Tiny RSS (M1 walking skeleton).
//!
//! Serves, under the configured base path (default `/tt-rss`):
//!   * static assets byte-for-byte from the doc root,
//!   * the login page (GET `/` and `/index.php` when not authenticated),
//!   * the login `POST` (`public.php?op=login`), including the TOTP challenge,
//!   * DB-backed encrypted sessions (read per request, persisted when changed),

//!   * `/healthz`.

use crate::core::phpser::Value;use crate::i18n::{__};

use crate::core::session as core_session;
use crate::core::config::Config;
use crate::util::{ct_eq, now_unix, php_int_cast};
use sqlx::Row;
use crate::{
    api, auth, diskcache, forgotpass, mime, nocache_headers, render, rpc, session, syndication,
    with_trailing_slash, AppState,
};
use axum::{
    body::Body,
    extract::{Extension, Form, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tokio::fs;


/// Redirect `<base>` → `<base>/` (Apache DirectorySlash parity).
/// Applied as a layer so it runs before route matching (the `nest` would
/// otherwise absorb the bare base path).
async fn base_slash_redirect(
    State(base): State<String>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_owned();
    if path == base {
        let target = format!("{base}/");
        let mut resp = Response::new(axum::body::Body::empty());
        *resp.status_mut() = axum::http::StatusCode::MOVED_PERMANENTLY;
        resp.headers_mut().insert(
            axum::http::header::LOCATION,
            axum::http::HeaderValue::from_str(&target).expect("valid url"),
        );
        return resp;
    }
    next.run(req).await
}

/// Build the axum router for the given app state (session middleware included).
pub fn build_router(state: &AppState) -> Router<AppState> {
    let base = state.config.base_path();

    let inner = Router::new()
        .route("/healthz", get(healthz))
        .route("/", get(index))
        .route("/index.php", get(index))
        .route("/prefs.php", get(prefs_page))
        .route("/public.php", get(public_handler).post(public_handler))
        .route("/backend.php", get(rpc::handler).post(rpc::handler))
        .route("/api/index.php", post(api::handler))
        // M5a.1: the tt-rss-android client POSTs to the bare directory URL
        // (`<ttrss_url>/api/`); Apache resolves it to `api/index.php` via
        // DirectoryIndex. Serve the handler directly at both `/api/` and `/api`
        // (the 301 the Apache would send for the slash-less form is
        // infrastructure, not tt-rss behavior).
        .route("/api", post(api::handler))
        .route("/api/", post(api::handler))
        .fallback(static_handler);

    let app = if base.is_empty() {
        inner
    } else {
        // Apache's DirectorySlash: 301 redirect <base> → <base>/ so relative
        // asset paths resolve correctly. The nest maps <base>/… to the inner
        // router; the explicit route serves <base>/ (the index).
        let base_slash = format!("{base}/");
        let base_for_redirect = base.clone();
        Router::new()
            .nest(&base, inner)
            .route(&base_slash, get(index))
            .layer(axum::middleware::from_fn_with_state(
                base_for_redirect,
                base_slash_redirect,
            ))
    };

    // Session middleware at the outer level so every route (inner + the
    // explicit trailing-slash index) gets the session handle + cookie re-stamp.
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        session::session_middleware,
    ))
}

async fn healthz() -> impl IntoResponse {
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain;charset=UTF-8")
        .body(Body::from("OK"))
        .expect("valid response")
}

/// GET `/` and `/index.php` — the authenticated SPA shell, or the login page
/// when the session has no logged-in user. The shell is revalidated via ETag.
async fn index(
    State(state): State<AppState>,
    Extension(sess): Extension<session::Sess>,
    headers: HeaderMap,
) -> Response {
    let data = sess.data();
    if !data.is_logged_in() {
        // Not authenticated → login page.
        let locale = crate::i18n::resolve_request_locale(&state.i18n, headers.get(axum::http::header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()), None);
        let html = crate::i18n::I18N
            .scope(
                crate::i18n::I18nCtx {
                    locale: locale.clone(),
                    cache: state.i18n.clone(),
                },
                async { render::render_login_page(&state.config) },
            )
            .await;
        return axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
            .header(header::CACHE_CONTROL, "private, no-cache")
            .body(Body::from(html))
            .expect("valid response");
    }
    // Authenticated → shell.
    let csrf = data.csrf_token().unwrap_or("");
    let ts = render::scripts_timestamp(state.config.docroot.as_path());
    let theme_link = theme_css_link(&state, &data).await;
    let user_css = user_css_style(&state, &data).await;
    let active_plugins = resolve_active_plugins(&state, &data).await;
    let (plugin_css, plugin_js) = plugin_assets(&state.plugins, &active_plugins);
    // M10.2: resolve locale for shell translations. Resolved before the plugin
    // toolbar so the toolbar can render inside the I18N scope (M10.4).
    let accept_language: Option<&str> = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    let uid = data.uid().unwrap_or(0);
    // PHP `include/functions.php:137`: `Prefs::get(USER_LANGUAGE, uid, profile)`
    // — resolve it through the standard pref path (profile override, then user
    // default), like every other user pref.
    let profile = data.profile().map(|p| p as i32);
    let rows = crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let user_lang_pref = crate::prefs::resolve("USER_LANGUAGE", &rows, profile);
    let user_lang = user_lang_pref.as_str();
    let locale = crate::i18n::resolve_request_locale(
        &state.i18n,
        accept_language,
        Some(user_lang),
    );
    let hide_logout = data
        .get_bool("hide_logout")
        .unwrap_or(false);
    let html = crate::i18n::I18N
        .scope(
            crate::i18n::I18nCtx {
                locale: locale.clone(),
                cache: state.i18n.clone(),
            },
            async {
                // M10.4: the plugin toolbar buttons render inside the I18N
                // scope so their strings resolve the request's locale (the
                // `__()` in each plugin hook reads the task-local).
                let main_toolbar_buttons = main_toolbar_buttons(&state, &sess).await;
                render::render_shell(
                    csrf,
                    ts,
                    &theme_link,
                    &user_css,
                    &plugin_css,
                    &plugin_js,
                    &main_toolbar_buttons,
                    hide_logout,
                )
            },
        )
        .await;
    let etag = format!("\"{}\"", md5_hex(&html));
    // `send_conditional_html`: 304 when the client's validator matches.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return axum::http::Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::CACHE_CONTROL, "private, no-cache")
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .expect("valid response");
    }
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(header::ETAG, &etag)
        .body(Body::from(html))
        .expect("valid response")
}

/// GET `/prefs.php` — the preferences SPA page (or the login page when not
/// authenticated, mirroring `UserHelper::login_sequence`).
async fn prefs_page(
    State(state): State<AppState>,
    Extension(sess): Extension<session::Sess>,
    headers: HeaderMap,
) -> Response {
    let data = sess.data();
    if !data.is_logged_in() {
        let locale = crate::i18n::resolve_request_locale(&state.i18n, headers.get(axum::http::header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()), None);
        let html = crate::i18n::I18N
            .scope(
                crate::i18n::I18nCtx {
                    locale: locale.clone(),
                    cache: state.i18n.clone(),
                },
                async { render::render_login_page(&state.config) },
            )
            .await;
        return axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
            .header(header::CACHE_CONTROL, "private, no-cache")
            .body(Body::from(html))
            .expect("valid response");
    }
    let csrf = data.csrf_token().unwrap_or("");
    let ts = render::scripts_timestamp(state.config.docroot.as_path());
    let access_level = data.access_level().unwrap_or(0);
    let theme_link = theme_css_link(&state, &data).await;
    let user_css = user_css_style(&state, &data).await;
    let active_plugins = resolve_active_plugins(&state, &data).await;
    // PHP `prefs.php` loops `get_prefs_css()`/`get_prefs_js()` — a different
    // pair than the shell's `get_css()`/`get_js()`.
    let (prefs_css, prefs_js) = prefs_plugin_assets(&state.plugins, &active_plugins);
    let prefs_plugin_js = if prefs_js.is_empty() {
        String::new()
    } else {
        format!("<script type=\"text/javascript\">\n\t\t{prefs_js}\n\t</script>")
    };
    let html = render::render_prefs_page(csrf, ts, access_level, &theme_link, &user_css, &prefs_plugin_js, &prefs_css);
    let etag = format!("W/\"{}\"", md5_hex(&html));
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(header::ETAG, etag)
        .body(Body::from(html))
        .expect("valid response")
}
/// The shell/prefs-page per-user theme `<link>` (PHP `index.php`/`prefs.php`
/// head): the `USER_CSS_THEME` pref resolved per (uid, profile), skipped in
/// safe mode, and only rendered when the theme file exists.
/// Resolve the active plugin names for this user (the PHP's
/// `init_plugins()`: system from `TTRSS_PLUGINS` + user from
/// `_ENABLED_PLUGINS`). Called once per request.
async fn resolve_active_plugins(state: &AppState, data: &core_session::Session) -> Vec<String> {
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let system: Vec<String> = state
        .config
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let pref_rows = crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let enabled =
        crate::prefs::resolve(crate::prefs::_ENABLED_PLUGINS, &pref_rows, profile).as_str().to_string();
    let user: Vec<String> = enabled
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    // Union of system + user plugins (the PHP loads both).
    let mut active: Vec<String> = system;
    for p in user {
        if !active.contains(&p) {
            active.push(p);
        }
    }
    active
}

/// Generate the active plugins' CSS and JS for the shell (the PHP's
/// Concatenate the `active` plugins' assets for one page: `css` joined into a
/// single block, each `js` wrapped in PHP's named try/catch. Pure registry
/// lookup — no DB access.
fn plugin_assets_for(
    host: &crate::pluginhost::PluginHost,
    active: &[String],
    css: impl Fn(&dyn crate::pluginhost::Plugin) -> String,
    js: impl Fn(&dyn crate::pluginhost::Plugin) -> String,
) -> (String, String) {
    let mut css_parts: Vec<String> = Vec::new();
    let mut js_parts: Vec<String> = Vec::new();
    for name in active {
        if let Some(plugin) = host.get_plugin(name) {
            let css = css(plugin);
            if !css.is_empty() {
                css_parts.push(css);
            }
            let js = js(plugin);
            if !js.is_empty() {
                js_parts.push(format!(
                    "try {{\n\t\t\t\t{js}\n\t\t\t}} catch (e) {{\n\t\t\t\t\tconsole.warn('failed to initialize plugin JS: {name}', e);\n\t\t\t}}"
                ));
            }
        }
    }
    (css_parts.join(""), js_parts.join(""))
}

/// The shell page's plugin assets (PHP `index.php` loop): `get_css`/`get_js`.
fn plugin_assets(host: &crate::pluginhost::PluginHost, active: &[String]) -> (String, String) {
    plugin_assets_for(host, active, |p| p.get_css(), |p| p.get_js())
}

/// The prefs page's plugin assets (PHP `prefs.php` loop): `get_prefs_css`/
/// `get_prefs_js` — a different pair than the shell's.
fn prefs_plugin_assets(host: &crate::pluginhost::PluginHost, active: &[String]) -> (String, String) {
    plugin_assets_for(host, active, |p| p.get_prefs_css(), |p| p.get_prefs_js())
}

/// PHP `index.php:195` — the main toolbar's plugin button row:
/// `run_hooks_callback(HOOK_MAIN_TOOLBAR_BUTTON, echo)`. The echo callback
/// only prints its result and never short-circuits, so a plain `run_hooks`
/// suffices; the concatenated `host.output` is the row (empty when no active
/// plugin emits a button).
async fn main_toolbar_buttons(state: &AppState, sess: &session::Sess) -> String {
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    pctx
        .run_hooks(
            crate::pluginhost::HOOK_MAIN_TOOLBAR_BUTTON,
            |plugin, host| plugin.hook_main_toolbar_button(host),
        )
        .await;
    pctx.output.trim().to_string()
}

async fn theme_css_link(state: &AppState, data: &core_session::Session) -> String {
    if data.safe_mode() {
        return String::new();
    }
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let rows = crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let pref = crate::prefs::resolve("USER_CSS_THEME", &rows, profile);
    let theme = pref.as_str();
    render::theme_css_link(state.config.docroot.as_path(), theme)
}

/// `UserHelper::print_user_stylesheet()` — the per-user custom CSS (the
/// "Customize" box under Preferences → General → Theme). Emitted as an inline
/// `<style id="user_css_style">` block in the shell + prefs head. Unlike
/// `theme_css_link`, it has no safe-mode guard: PHP prints it whenever the
/// `USER_STYLESHEET` pref is non-empty.
async fn user_css_style(state: &AppState, data: &core_session::Session) -> String {
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let rows = crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let value = crate::prefs::resolve(crate::prefs::USER_STYLESHEET, &rows, profile)
        .as_str()
        .to_string();
    if value.is_empty() {
        return String::new();
    }
    format!(
        "<style type='text/css' id='user_css_style'>{}</style>",
        value.replace("<br/>", "\n")
    )
}

/// `md5` of a byte slice, lowercase hex (for the shell ETag `"<md5>"`).
fn md5_hex(data: &str) -> String {
    let digest = md5::compute(data.as_bytes());
    let mut s = String::with_capacity(32);
    for b in digest.as_slice() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// POST/GET `public.php` — dispatch on `op`. M1 handles `op=login` and
/// `op=logout`; other ops (rss, feed_icon, forgotpass, ...) land in M2+.
async fn public_handler(
    State(state): State<AppState>,
    sess: Extension<session::Sess>,
    headers: axum::http::HeaderMap,
    query: Query<PublicQuery>,
    form: Form<PublicForm>,
) -> Response {
    // The `$_SERVER`-equivalent (M7.20 `auth_remote`): the raw header
    // names/values (case as sent) + the Basic-auth decoded user.
    let server_env: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    // `$_REQUEST['op']` merges GET + POST, so `op` may arrive in either.
    let op = query.op.as_deref().or_else(|| form.op.as_deref());
    // `public.php` shortcut: `?op=<plugin>--<pmethod>` is rewritten to
    // `op=pluginhandler` with `plugin`/`pmethod` injected into `$_REQUEST`
    // (`PluginHost::PUBLIC_METHOD_DELIMITER`).
    let (op, shortcut) = match op.and_then(|o| o.split_once("--")) {
        Some((plugin, pmethod)) => (
            "pluginhandler",
            Some((plugin.to_string(), pmethod.to_string())),
        ),
        None => (op.unwrap_or(""), None),
    };
    // The request's `Accept-Language` (M10.3): threaded to the syndication
    // handler, which resolves it into the UI locale for the feed titles.
    let accept_language = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    match op {
        "login" => {
            let env: Vec<(String, String)> = basic_auth_env(&server_env)
                .into_iter()
                .chain(server_env)
                .collect();
            login_flow(&state, sess.0, &query, &form, &env).await
        }
        "logout" => logout_flow(&state, sess.0, &form).await,
        // `Handler_Public::pluginhandler` (M7.3) — the no-account public
        // plugin-method endpoint; the only gate is the plugin's
        // `is_public_method`.
        "pluginhandler" => {
            let (plugin, pmethod) = match shortcut {
                Some((p, m)) => (p, m),
                None => (
                    query
                        .plugin
                        .as_deref()
                        .or_else(|| form.plugin.as_deref())
                        .unwrap_or("")
                        .to_string(),
                    query
                        .pmethod
                        .as_deref()
                        .or_else(|| form.pmethod.as_deref())
                        .unwrap_or("")
                        .to_string(),
                ),
            };
            let request = public_request_map(&query, &form);
            match crate::pluginhost::dispatch::public_dispatch(&state, &plugin, &pmethod, &request)
                .await
            {
                Ok(crate::pluginhost::plugin::MethodOutput::Json(v)) =>
                    (StatusCode::OK, v.to_string()).into_response(),
                Ok(crate::pluginhost::plugin::MethodOutput::Raw(s)) =>
                    (StatusCode::OK, s).into_response(),
                // A full response (custom status + Content-Type) — `share::get`.
                Ok(crate::pluginhost::plugin::MethodOutput::Response {
                    status,
                    content_type,
                    body,
                }) => (
                    StatusCode::from_u16(status).unwrap(),
                    [(header::CONTENT_TYPE, content_type)],
                    body,
                )
                    .into_response(),
                // PHP: an uncaught exception in the public handler — 500.
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        // `Handler_Public::forgotpass` — the public password-recovery page.
        "forgotpass" => {
            let p = forgotpass::Params {
                hash: query.hash.as_deref().or_else(|| form.hash.as_deref()),
                login: query
                    .login
                    .as_deref()
                    .or_else(|| form.login.as_deref())
                    .unwrap_or(""),
                email: form.email.as_deref(),
                test: form.test.as_deref(),
                csrf_token: form.csrf_token.as_deref(),
                method: form.method.as_deref(),
            };
            forgotpass::forgotpass(&state, &sess.0, &p).await
        }
        // `Handler_Public::cached` — serve `<CACHE_DIR>/<subdir>/<file>`.
        "cached" => cached_file(&state, query.file.as_deref()),
        // `Handler_Public::feed_icon` — serve `<CACHE_DIR>/feed-icons/<id>`.
        "feed_icon" => query
            .id
            .as_deref()
            .map(|id| feed_icon_response(&state.config, id))
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response()),
        // `Handler_Public::rss` — syndicated Atom/JSON feed (access-key gated).
        "rss" => {
            let req = syndication::RssRequest {
                id: query.id.as_deref(),
                key: query.key.as_deref(),
                is_cat: param_to_bool(query.is_cat.as_deref()),
                limit: clean_int(query.limit.as_deref()),
                offset: clean_int(query.offset.as_deref()),
                search: query.q.as_deref().unwrap_or(""),
                view_mode: query.view_mode.as_deref().unwrap_or(""),
                order: query.order.as_deref().unwrap_or(""),
                start_ts: query.ts.as_deref().unwrap_or(""),
                format: query.format.as_deref().unwrap_or("atom"),
                orig_guid: query.orig_guid.as_deref().unwrap_or(""),
                noxml: param_to_bool(query.noxml.as_deref()),
                accept_language,
            };
            syndication::handle_rss(&state, &req).await
        }
        // `Handler_Public::getProfiles` — the login page's Profile dropdown.
        // The login form POSTs `op=getprofiles&login=…` in the form body on
        // login-field change / focus / blur; PHP reads `$_REQUEST["login"]`
        // (GET + POST merged), so the param is accepted from either.
        "getprofiles" => {
            let login = query
                .login
                .as_deref()
                .or_else(|| form.login.as_deref())
                .unwrap_or("");
            let v = auth::get_profiles(&state.db, login).await;
            (StatusCode::OK, v.to_string()).into_response()
        }
        // `Handler_Public::healthcheck` — the monitoring probe (same body as
        // the `/healthz` route).
        "healthcheck" => healthz().await.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `Handler_Public::cached()`: `?file=<subdir>/<name>` from the app cache dir.
fn cached_file(state: &AppState, file: Option<&str>) -> Response {
    match file.and_then(|f| diskcache::resolve_cached(&state.config.cache_dir, f)) {
        Some(path) => diskcache::send_file(&path),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `Handler_Public::feed_icon` — serve `<CACHE_DIR>/feed-icons/<id>`, or 404.
///
/// The reference does `$id = (int)$_REQUEST['id']`; the "Upload new icon"
/// preview URL carries a cache-buster appended *after* `&id=…` (e.g.
/// `…public.php?op=feed_icon&id=12?ts=1699…`), so the raw `id` param is
/// `12?ts=1699…`. Cast it to an integer first (PHP `(int)`) so the lookup opens
/// `feed-icons/12` rather than a literal `feed-icons/12?ts=…` path that 404s —
/// which is exactly what left the upload preview a broken image.
fn feed_icon_response(cfg: &Config, id: &str) -> Response {
    let path = cfg.feed_icon_dir().join(php_int_cast(id).to_string());
    if path.file_name().is_some() {
        diskcache::send_file(&path)
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// `Handler::_param_to_bool($p)`: `clean($p) && ($p !== "f" && $p !== "false")`.
fn param_to_bool(v: Option<&str>) -> bool {
    v.map(|s| {
        let val = serde_json::Value::String(s.to_string());
        crate::util::param_to_bool(Some(&val))
    })
    .unwrap_or(false)
}

/// PHP `(int)clean(...)`: the leading integer prefix of the string
/// (`""`/non-numeric → `0`).
fn clean_int(v: Option<&str>) -> i64 {
    let s = v.unwrap_or("").trim();
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '-' && c != '+')
        .unwrap_or(s.len());
    s.get(..end)
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct PublicQuery {
    op: Option<String>,
    file: Option<String>,
    id: Option<String>,
    #[serde(rename = "return")]
    return_url: Option<String>,
    // `op=forgotpass` (password recovery) params — the reset link arrives via
    // `?op=forgotpass&hash=…&login=…` (GET).
    hash: Option<String>,
    login: Option<String>,
    // `op=rss` (syndication) params
    key: Option<String>,
    format: Option<String>,
    is_cat: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
    q: Option<String>,
    #[serde(rename = "view-mode")]
    view_mode: Option<String>,
    order: Option<String>,
    ts: Option<String>,
    orig_guid: Option<String>,
    noxml: Option<String>,
    // `op=pluginhandler` (public plugin methods, M7.3) params.
    plugin: Option<String>,
    pmethod: Option<String>,
    /// Any params the struct doesn't name (the plugin method's `$_REQUEST`
    /// must see them all).
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct PublicForm {
    op: Option<String>,
    login: Option<String>,
    password: Option<String>,
    otp: Option<String>,
    remember_me: Option<String>,
    safe_mode: Option<String>,
    bw_limit: Option<String>,
    profile: Option<String>,
    csrf_token: Option<String>,
    // `op=forgotpass` (password recovery) form fields (POST `method=do`).
    // `hash` is `$_REQUEST` in PHP (the reset link is a GET, but the param is
    // accepted from the form body as well).
    hash: Option<String>,
    email: Option<String>,
    test: Option<String>,
    method: Option<String>,
    // `op=pluginhandler` (public plugin methods, M7.3) params — `$_REQUEST`
    // merges the form body too.
    plugin: Option<String>,
    pmethod: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// PHP's `$_REQUEST` as a public-endpoint plugin method sees it: GET + POST
/// merged (form fields win, as `$_REQUEST` on a POST). Named struct fields
/// are merged with the `extra` catch-all so the method sees every request
/// parameter (the port's substitute for reading the superglobals).
fn public_request_map(query: &PublicQuery, form: &PublicForm) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (k, v) in query.extra.iter() {
        m.insert(k.clone(), v.clone());
    }
    for (k, v) in form.extra.iter() {
        m.insert(k.clone(), v.clone());
    }
    let named: &[(&str, Option<&str>, Option<&str>)] = &[
        ("op", query.op.as_deref(), form.op.as_deref()),
        ("file", query.file.as_deref(), None),
        ("id", query.id.as_deref(), None),
        ("return", query.return_url.as_deref(), None),
        ("hash", query.hash.as_deref(), form.hash.as_deref()),
        ("login", query.login.as_deref(), form.login.as_deref()),
        ("key", query.key.as_deref(), None),
        ("format", query.format.as_deref(), None),
        ("is_cat", query.is_cat.as_deref(), None),
        ("limit", query.limit.as_deref(), None),
        ("offset", query.offset.as_deref(), None),
        ("q", query.q.as_deref(), None),
        ("view-mode", query.view_mode.as_deref(), None),
        ("order", query.order.as_deref(), None),
        ("ts", query.ts.as_deref(), None),
        ("orig_guid", query.orig_guid.as_deref(), None),
        ("noxml", query.noxml.as_deref(), None),
        ("plugin", query.plugin.as_deref(), form.plugin.as_deref()),
        ("pmethod", query.pmethod.as_deref(), form.pmethod.as_deref()),
        ("password", None, form.password.as_deref()),
        ("otp", None, form.otp.as_deref()),
        ("remember_me", None, form.remember_me.as_deref()),
        ("safe_mode", None, form.safe_mode.as_deref()),
        ("bw_limit", None, form.bw_limit.as_deref()),
        ("profile", None, form.profile.as_deref()),
        ("csrf_token", None, form.csrf_token.as_deref()),
        ("email", None, form.email.as_deref()),
        ("test", None, form.test.as_deref()),
        ("method", None, form.method.as_deref()),
    ];
    for (name, q, f) in named {
        if let Some(v) = (*f).or(*q) {
            m.insert((*name).to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::Value::Object(m)
}

/// `Handler_Public::logout` + `UserHelper::logout`.
async fn logout_flow(state: &AppState, sess: session::Sess, form: &PublicForm) -> Response {
    // `validate_csrf`: `isset($csrf) && hash_equals($_SESSION['csrf_token'] ?? "", $csrf)`.
    let data = sess.data();
    let session_csrf = data.csrf_token().unwrap_or("");
    let valid = form
        .csrf_token
        .as_deref()
        .map(|c| ct_eq(session_csrf, c))
        .unwrap_or(false);
    if !valid {
        let mut res = axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"error":{"code":"E_UNAUTHORIZED","params":[]}}"#,
            ))
            .expect("valid response");
        if sess.had_cookie() {
            nocache_headers(res.headers_mut());
        }
        return res;
    }
    let (u_login, u_id) = {
        let d = sess.data();
        (d.get_str("name").unwrap_or("").to_string(), d.uid().unwrap_or(0))
    };
    let u_login_captured = u_login.clone();
    let sid = sess.id();
    sess.destroy();
    // `HOOK_POST_LOGOUT` (PHP `Handler_Public::logout`): the first non-empty
    // `[0]` (validated) is the redirect target; otherwise the default.
    let mut redirect_url = String::new();
    {
        let mut ctx = crate::pluginhost::dispatch::login_ctx(state);
        ctx.run_hooks_callback(
            crate::pluginhost::HOOK_POST_LOGOUT,
            |plugin, ctx| {
                let login = u_login_captured.clone();
                Box::pin(async move { plugin.hook_post_logout(ctx, &login, u_id as i32).await })
            },
            |result: &serde_json::Value, _plugin| {
                if let Some(url) = result.get(0).and_then(|v| v.as_str())
                    && !url.is_empty() && redirect_url.is_empty() {
                        redirect_url = url.to_string();
                    }
                false
            },
        )
        .await;
    }
    let location = if redirect_url.is_empty() {
        format!("{}/index.php", state.config.self_url_rtrim())
    } else {
        crate::urlhelper::validate(&redirect_url, false).unwrap_or_else(|| {
            format!("{}/index.php", state.config.self_url_rtrim())
        })
    };
    let mut res = axum::http::Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
        .body(Body::empty())
        .expect("valid response");
    // The session was active: the PHP nocache limiter stamped the response.
    if sess.had_cookie() {
        nocache_headers(res.headers_mut());
    }
    // `extend_session` (request start) re-stamped the cookie, then
    // `UserHelper::logout` queued the deletion cookie — in that order.
    if !sid.is_empty() {
        let expires = session::rfc1123(now_unix() + session::COOKIE_LIFETIME);
        res.headers_mut().append(
            header::SET_COOKIE,
            format!(
                "{}={sid}; expires={expires}; Max-Age={}; path=/",
                session::SESSION_NAME,
                session::COOKIE_LIFETIME
            )
            .parse()
            .expect("valid set-cookie"),
        );
        res.headers_mut().append(
            header::SET_COOKIE,
            format!(
                "{}=deleted; expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0; path=/",
                session::SESSION_NAME
            )
            .parse()
            .expect("valid set-cookie"),
        );
    }
    res
}

/// The `PHP_AUTH_USER`-equivalent: the decoded Basic-auth username (PHP's
/// Apache `mod_auth` sets it; the port decodes `Authorization: Basic …`
/// itself). `None` when absent/malformed.
fn basic_auth_env(env: &[(String, String)]) -> Vec<(String, String)> {
    let basic = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
        .and_then(|(_, v)| v.as_str().strip_prefix("Basic "))
        .and_then(|b| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(b)
                .ok()
                .and_then(|raw| String::from_utf8(raw).ok())
                .and_then(|s| s.split_once(':').map(|(user, _)| user.to_string()))
        });
    basic
        .map(|u| vec![(String::from("PHP_AUTH_USER"), u)])
        .unwrap_or_default()
}

/// `Handler_Public::login` + the `HOOK_AUTH_USER` chain (M7.20).
async fn login_flow(
    state: &AppState,
    sess: session::Sess,
    query: &PublicQuery,
    form: &PublicForm,
    server_env: &[(String, String)],
) -> Response {
    let login = form.login.as_deref().unwrap_or("").to_string();
    let password = form.password.as_deref().unwrap_or("").to_string();
    let _otp = form
        .otp
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok());
    let return_url = query.return_url.as_deref().unwrap_or("");
    let self_url = state.config.self_url_rtrim();

    // M7.20: go through the plugin system (HOOK_AUTH_USER) instead of
    // calling auth::authenticate directly. The first plugin that returns
    // Some(uid) wins; the OTP challenge / failure message are carried on
    // the HostCtx side-channels.
    // M7.20: the auth chain runs over the **system** plugins only (PHP
    // `load_all(KIND_SYSTEM, 0, true)` — per-user plugins never
    // participate in authentication).
    let mut ctx = crate::pluginhost::dispatch::login_ctx(state)
        .with_server_env(server_env.to_vec());
    let mut uid: Option<i32> = None;
    let mut auth_module: Option<&str> = None;
    let host = ctx.host_arc();
    for idx in ctx.hook_order(crate::pluginhost::HOOK_AUTH_USER) {
        let plugin = host.plugin(idx);
        match plugin.authenticate(&mut ctx, &login, &password, "").await {
            Some(id) => {
                uid = Some(id);
                // `$_SESSION["auth_module"] = strtolower($plugin::class)` —
                // the plugin that succeeded.
                auth_module = Some(plugin.class_name());
                break;
            }
            None => {
                if ctx.otp_challenge {
                    break;
                }
            }
        }
    }

    match uid {
        Some(id) => {
            // Re-read the user row for the session setup (the plugin only
            // returns the uid; the session needs login/access_level/pwd_hash).
            let row = sqlx::query(
                "SELECT login, access_level, pwd_hash FROM ttrss_users WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(ctx.pool())
            .await
            .ok()
            .flatten();
            if let Some(r) = row {
                let u_login: String = r.try_get("login").unwrap_or_default();
                let access_level: i32 = r.try_get("access_level").unwrap_or(0);
                let pwd_hash: String = r.try_get("pwd_hash").unwrap_or_default();
                let (new_id, data) = auth::build_login_session(
                    id,
                    &u_login,
                    access_level,
                    &pwd_hash,
                    truthy(form.bw_limit.as_deref()),
                    checkbox(form.safe_mode.as_deref()),
                    auth_module.unwrap_or("auth_internal"),
                );
                // `auth_remote`'s fake-login markers (PHP `$_SESSION[
                // "fake_login"/"fake_password"/"hide_hello"/"hide_logout"]`):
                // the session payload is JSON, so the extra keys ride along
                // (the JS reads `fake_login`/`hide_logout` for the login
                // page's "logged in as …" line + logout-button hiding).
                let mut data = data;
                if let Some(fake) = ctx.fake_login {
                    use crate::core::phpser::Value;
                    data.set("fake_login", Value::Bytes(fake.into_bytes()));
                    data.set("fake_password", Value::Bytes(b"******".to_vec()));
                    data.set("hide_hello", Value::Bool(true));
                    data.set("hide_logout", Value::Bool(true));
                }
                sess.replace(new_id.clone(), data);
                return login_redirect(&location(return_url, &self_url), &new_id);
            }
        }
        None => {
            if ctx.otp_challenge {
                let enc = render::urlencode(&with_trailing_slash(return_url));
                let otp_form = render::OtpForm {
                    login: login.clone(),
                    password: password.clone(),
                    bw_limit: form.bw_limit.clone().unwrap_or_default(),
                    safe_mode: form.safe_mode.clone().unwrap_or_default(),
                    remember_me: form.remember_me.clone().unwrap_or_default(),
                    profile: form.profile.clone().unwrap_or_default(),
                };
                let html = render::render_otp_challenge(&state.config, &enc, &otp_form);
                return axum::http::Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
                    .body(Body::from(html))
                    .expect("valid response");
            }
        }
    }

    // Failure (or no plugin handled it): set the error and redirect.
    let message = ctx.login_error.unwrap_or_else(|| {
        __("Incorrect username or password")
    });
    let cur = sess.id();
    let sid = if cur.is_empty() {
        core_session::new_session_id()
    } else {
        cur
    };
    let mut data = sess.data();
    data.set("login_error_msg", Value::Bytes(message.into_bytes()));
    sess.replace(sid.clone(), data);
    login_redirect(&location(return_url, &self_url), &sid)
}

/// The `Location` for a login redirect: the validated `return` URL, else the
/// app's `index.php` (the reference oracle redirects to `…/tt-rss/index.php`).
fn location(return_url: &str, self_url: &str) -> String {
    if !return_url.is_empty() && matches_self_url(return_url, self_url) {
        return_url.to_string()
    } else {
        format!("{self_url}/index.php")
    }
}

fn login_redirect(location: &str, sid: &str) -> Response {
    // `public.php` goes through the PHP session, whose default `nocache`
    // limiter stamps the response with these cache-busting headers.
    axum::http::Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(
            header::SET_COOKIE,
            format!("{}={sid}; path=/", session::SESSION_NAME),
        )
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .header(header::PRAGMA, "no-cache")
        .header(header::EXPIRES, "Thu, 19 Nov 1981 08:52:00 GMT")
        .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
        .body(Body::empty())
        .expect("valid response")
}

/// `Config::matches_self_url`: same scheme/host/port and (for a non-root base
/// path) an exact or prefixed path match.
fn matches_self_url(url: &str, self_url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Ok(s) = url::Url::parse(self_url) else {
        return false;
    };
    if u.scheme() != s.scheme()
        || u.host_str() != s.host_str()
        || u.port_or_known_default() != s.port_or_known_default()
    {
        return false;
    }
    let self_path = s.path().trim_end_matches('/');
    if self_path.is_empty() {
        return true;
    }
    let u_path = u.path().trim_end_matches('/');
    u_path == self_path || u_path.starts_with(&format!("{self_path}/"))
}

/// `!!clean($x)`: truthy unless empty or "0".
fn truthy(s: Option<&str>) -> bool {
    !matches!(s, None | Some("") | Some("0"))
}

/// `checkbox_to_sql_bool`: checked checkboxes post "1"/"on".
fn checkbox(s: Option<&str>) -> bool {
    matches!(s, Some("1") | Some("on"))
}

/// Catch-all static asset handler: serves a file from the doc root
/// byte-for-byte with the oracle's static-file headers — correct Content-Type
/// (from the nginx `mime.types`), `Cache-Control: no-cache`, and no
/// `Accept-Ranges` (nginx omits it on the gzip/chunked path real clients hit).
async fn static_handler(State(state): State<AppState>, req: Request) -> axum::response::Response {
    let base = state.config.base_path();
    let uri_path = req.uri().path();
    let rel = if base.is_empty() {
        uri_path.to_string()
    } else {
        uri_path
            .strip_prefix(base.as_str())
            .unwrap_or(uri_path)
            .to_string()
    };
    let rel = rel.trim_start_matches('/');
    let candidate = state.config.docroot.join(rel);
    // Resolve and confine to the doc root (defends against path traversal).
    match (
        fs::canonicalize(&candidate).await,
        fs::canonicalize(&state.config.docroot).await,
    ) {
        (Ok(file), Ok(docroot)) if file.starts_with(&docroot) && file.is_file() => {
            match fs::read(&file).await {
                Ok(bytes) => {
                    let mime = mime::mime_for_path(&file);
                    let mut res = axum::response::Response::new(Body::from(bytes));
                    res.headers_mut()
                        .insert(header::CONTENT_TYPE, mime.parse().unwrap());
                    res.headers_mut()
                        .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
                    res
                }
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Config::matches_self_url` — the `urlMatchDataProvider` from
    /// tt-rss/tests_functional/ConfigTest.php, ported 1:1 (self_url, url_to_check,
    /// expected).
    #[test]
    fn matches_self_url_provider() {
        let cases: &[(&str, &str, bool)] = &[
            // root origin matches
            ("https://example.com", "https://example.com", true),
            ("https://example.com", "https://example.com/", true),
            ("https://example.com", "https://example.com/any/path", true),
            ("https://example.com", "HTTPS://EXAMPLE.COM", true),
            ("https://example.com", "https://example.com:443", true),
            ("http://example.com", "http://example.com:80", true),
            // root origin mismatches
            ("https://example.com", "http://example.com", false),
            ("https://example.com", "https://attacker.com", false),
            ("https://example.com", "https://example.com:8443", false),
            ("https://example.com", "https://sub.example.com", false),
            ("https://example.com", "invalid-url", false),
            // path prefix matches
            (
                "https://example.com/tt-rss",
                "https://example.com/tt-rss",
                true,
            ),
            (
                "https://example.com/tt-rss",
                "https://example.com/tt-rss/",
                true,
            ),
            (
                "https://example.com/tt-rss",
                "https://example.com/tt-rss/api/feeds",
                true,
            ),
            (
                "https://example.com/apps/rss",
                "https://example.com/apps/rss/index.php",
                true,
            ),
            // path prefix mismatches
            ("https://example.com/tt-rss", "https://example.com", false),
            (
                "https://example.com/tt-rss",
                "https://example.com/tt-rss-malicious",
                false,
            ),
            (
                "https://example.com/tt-rss",
                "https://example.com/other-path",
                false,
            ),
        ];
        for (self_url, url, expected) in cases {
            assert_eq!(
                matches_self_url(url, self_url),
                *expected,
                "self={self_url} url={url}"
            );
        }
    }

    // NOTE: the PHP `matches_self_url` also IDN-normalizes hosts (Punycode vs
    // Unicode, `idn_to_ascii(..., UTS46)`) — `url::Url` keeps the host as
    // written, so 'https://xn--mller-kva.com' vs 'https://müller.com' is not
    // matched here. REVISIT (IDN normalization) — tracked in docs/01-plan.md.

    // NOTE: the `test_cast_to_int/string/bool` cases from ConfigTest.php exercise
    // `Config::cast_to`, whose logic this port keeps in `prefs::cast` — already
    // covered there (`prefs::tests::cast_string_to_int` / `cast_string_to_bool` /
    // `cast_bool_zero_string_is_false`).

    // REVISIT (REVISIT): the `get_self_url` (a–k) and `get_self_dir` cases from
    // ConfigTest.php exercise the PHP `Config::get_self_url()` (which detects the
    // self URL from $_SERVER / is_server_https) and `Config::get_self_dir()`.
    // The Rust port takes SELF_URL_PATH + docroot from the environment instead,
    // so these are N/A here. Tracked in docs/01-plan.md (test-parity, config row).

    // FunctionsTest.php — ported 1:1 (tracked in docs/01-plan.md, test-parity,
    // functions row): clean (10, server::util), truncate_string (11,
    // headlines::tests), truncate_middle (7), mb_substr_replace (6),
    // arr_qmarks (6), uniqid_short (4), get_random_bytes (3), t_sprintf (2),
    // t_nsprintf (1), gzdecode (1) — all in server::util::tests; bool_to_sql_bool
    // (2), checkbox_to_sql_bool (2), sql_bool_to_bool (3) — core::config::tests;
    // get_theme_path (3) + get_scripts_timestamp (2) — render::tests;
    // theme_exists (1) — rpc::tests; with_trailing_slash (5) — above.
    //
    // REVISIT (parked): implements_interface (4 cases) — PHP
    // `class_implements()` reflection with no Rust consumer in the port yet.
    // Revisit if a plugin/handler type-check surface is actually needed.

    /// PHP `index.php` vs `prefs.php`: the shell page takes the active
    /// plugins' `get_css`/`get_js`, the prefs page their `get_prefs_css`/
    /// `get_prefs_js`. With `share` active, the two pairs are disjoint:
    /// `share.js` has `shareArticle`, `share_prefs.js` has `clearKeys`.
    /// Regression: the port once fed the shell pair to the prefs page.
    #[test]
    fn page_assets_use_their_own_plugin_pair() {
        let host = crate::pluginhost::PluginHost::startup(&[
            || Box::new(crate::plugins::share::Share) as Box<dyn crate::pluginhost::Plugin>,
        ]);
        let active = vec!["share".to_string()];

        let (shell_css, shell_js) = plugin_assets(&host, &active);
        assert!(shell_js.contains("shareArticle"), "shell must get share.js");
        assert!(!shell_js.contains("clearKeys"), "prefs JS must not leak into the shell");

        let (prefs_css, prefs_js) = prefs_plugin_assets(&host, &active);
        assert!(prefs_js.contains("clearKeys"), "prefs must get share_prefs.js");
        assert!(!prefs_js.contains("shareArticle"), "shell JS must not leak into the prefs page");

        // `share.css` exists, `share_prefs.css` does not — the shell gets the
        // CSS, the prefs page correctly gets none.
        assert!(!shell_css.is_empty(), "share.css expected in the shell");
        assert!(prefs_css.is_empty(), "share has no share_prefs.css");
    }

    /// Regression for the broken "Upload new icon" preview: the preview URL
    /// the `uploadIcon` handler builds is malformed — the `?ts=` cache-buster
    /// is appended *after* `&id=…`, so the raw `id` query param is e.g.
    /// `12?ts=1699…`. `feed_icon_response` must cast that to an integer (PHP
    /// `(int)`, mirroring `Handler_Public::feed_icon`) so it resolves to
    /// `feed-icons/12`; treating the raw string as a filename 404'd (the bug).
    #[test]
    fn feed_icon_resolves_the_malformed_preview_id() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        // A 1×1 transparent PNG, so `send_file`'s media-type guard passes → 200.
        let png = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
            .unwrap();
        let dir = std::env::temp_dir().join(format!("ttrss-feedicon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let icon_dir = dir.join("feed-icons");
        std::fs::create_dir_all(&icon_dir).unwrap();
        std::fs::write(icon_dir.join("12"), &png).unwrap();
        let cfg = Config {
            cache_dir: dir.clone(),
            ..Default::default()
        };

        // The real upload-preview shape: `…&id=12?ts=1699…` → raw id `12?ts=…`.
        assert_eq!(
            feed_icon_response(&cfg, "12?ts=1790567389").status(),
            StatusCode::OK,
            "malformed id?ts=… must serve feed-icons/12, not 404"
        );
        // A clean id also works.
        assert_eq!(feed_icon_response(&cfg, "12").status(), StatusCode::OK);
        // A feed with no cached icon 404s.
        assert_eq!(
            feed_icon_response(&cfg, "999").status(),
            StatusCode::NOT_FOUND
        );
        // Non-numeric / traversal ids collapse to 0 → no file → 404 (no escape).
        assert_eq!(
            feed_icon_response(&cfg, "../etc/passwd").status(),
            StatusCode::NOT_FOUND
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
