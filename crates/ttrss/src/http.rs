//! `ttrss` — the Rust HTTP server for Tiny Tiny RSS (M1 walking skeleton).
//!
//! Serves, under the configured base path (default `/tt-rss`):
//!   * static assets byte-for-byte from the doc root,
//!   * the login page (GET `/` and `/index.php` when not authenticated),
//!   * the login `POST` (`public.php?op=login`), including the TOTP challenge,
//!   * DB-backed encrypted sessions (read per request, persisted when changed),

//!   * `/healthz`.

use crate::core::phpser::Value;
use crate::core::session as core_session;
use crate::util::{ct_eq, now_unix};
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
        // `nest` maps `<base>` and `<base>/…` but not `<base>/` (trailing
        // slash) to the inner root, so serve the index there explicitly.
        let base_slash = format!("{base}/");
        Router::new()
            .nest(&base, inner)
            .route(&base_slash, get(index))
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
        let html = render::render_login_page(&state.config);
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
    let html = render::render_shell(csrf, ts, &theme_link);
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
) -> Response {
    let data = sess.data();
    if !data.is_logged_in() {
        let html = render::render_login_page(&state.config);
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
    let html = render::render_prefs_page(csrf, ts, access_level, &theme_link);
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
async fn theme_css_link(state: &AppState, data: &core_session::Session) -> String {
    if data.safe_mode() {
        return String::new();
    }
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let rows = crate::prefs::load_rows(&state.db, uid, profile).await;
    let pref = crate::prefs::resolve("USER_CSS_THEME", &rows, profile);
    let theme = pref.as_str();
    render::theme_css_link(state.config.docroot.as_path(), theme)
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
    query: Query<PublicQuery>,
    form: Form<PublicForm>,
) -> Response {
    // `$_REQUEST['op']` merges GET + POST, so `op` may arrive in either.
    let op = query.op.as_deref().or_else(|| form.op.as_deref());
    match op {
        Some("login") => login_flow(&state, sess.0, &query, &form).await,
        Some("logout") => logout_flow(&state, sess.0, &form).await,
        // `Handler_Public::forgotpass` — the public password-recovery page.
        Some("forgotpass") => {
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
        Some("cached") => cached_file(&state, query.file.as_deref()),
        // `Handler_Public::feed_icon` — serve `<CACHE_DIR>/feed-icons/<id>`.
        Some("feed_icon") => match query
            .id
            .as_deref()
            .map(|id| state.config.feed_icon_dir.join(id.trim()))
        {
            Some(path) if path.file_name().is_some() => diskcache::send_file(&path),
            _ => StatusCode::NOT_FOUND.into_response(),
        },
        // `Handler_Public::rss` — syndicated Atom/JSON feed (access-key gated).
        Some("rss") => {
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
            };
            syndication::handle_rss(&state, &req).await
        }
        // `Handler_Public::getProfiles` — the login page's Profile dropdown.
        // The login form POSTs `op=getprofiles&login=…` in the form body on
        // login-field change / focus / blur; PHP reads `$_REQUEST["login"]`
        // (GET + POST merged), so the param is accepted from either.
        Some("getprofiles") => {
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
        Some("healthcheck") => healthz().await.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `Handler_Public::cached()`: `?file=<subdir>/<name>` from the app cache dir.
fn cached_file(state: &AppState, file: Option<&str>) -> Response {
    match file.and_then(|f| diskcache::resolve_cached(&state.config.feed_icon_dir, f)) {
        Some(path) => diskcache::send_file(&path),
        None => StatusCode::NOT_FOUND.into_response(),
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
    let sid = sess.id();
    sess.destroy();
    let mut res = axum::http::Response::builder()
        .status(StatusCode::FOUND)
        .header(
            header::LOCATION,
            format!("{}/index.php", state.config.self_url_rtrim()),
        )
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

/// `Handler_Public::login` + `UserHelper::authenticate` (web, auth_internal).
async fn login_flow(
    state: &AppState,
    sess: session::Sess,
    query: &PublicQuery,
    form: &PublicForm,
) -> Response {
    let login = form.login.as_deref().unwrap_or("").to_string();
    let password = form.password.as_deref().unwrap_or("").to_string();
    let otp = form
        .otp
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok());
    let return_url = query.return_url.as_deref().unwrap_or("");
    let self_url = state.config.self_url_rtrim();

    match auth::authenticate(&state.db, &login, &password, otp).await {
        auth::AuthOutcome::Success {
            id,
            login,
            access_level,
            pwd_hash,
        } => {
            let (new_id, data) = auth::build_login_session(
                id,
                &login,
                access_level,
                &pwd_hash,
                truthy(form.bw_limit.as_deref()),
                checkbox(form.safe_mode.as_deref()),
            );
            sess.replace(new_id.clone(), data);
            login_redirect(&location(return_url, &self_url), &new_id)
        }
        auth::AuthOutcome::OtpChallenge => {
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
            axum::http::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
                .body(Body::from(html))
                .expect("valid response")
        }
        auth::AuthOutcome::Failure { message } => {
            let cur = sess.id();
            let id = if cur.is_empty() {
                core_session::new_session_id()
            } else {
                cur
            };
            let mut data = sess.data();
            data.set("login_error_msg", Value::Bytes(message.into_bytes()));
            sess.replace(id.clone(), data);
            login_redirect(&location(return_url, &self_url), &id)
        }
    }
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
    // matched here. REVISIT (IDN normalization) — tracked in docs/plan.md.

    // NOTE: the `test_cast_to_int/string/bool` cases from ConfigTest.php exercise
    // `Config::cast_to`, whose logic this port keeps in `prefs::cast` — already
    // covered there (`prefs::tests::cast_string_to_int` / `cast_string_to_bool` /
    // `cast_bool_zero_string_is_false`).

    // REVISIT (REVISIT): the `get_self_url` (a–k) and `get_self_dir` cases from
    // ConfigTest.php exercise the PHP `Config::get_self_url()` (which detects the
    // self URL from $_SERVER / is_server_https) and `Config::get_self_dir()`.
    // The Rust port takes SELF_URL_PATH + docroot from the environment instead,
    // so these are N/A here. Tracked in docs/plan.md (test-parity, config row).

    // FunctionsTest.php — ported 1:1 (tracked in docs/plan.md, test-parity,
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
}
