//! `tt-rust` — the Rust HTTP server for Tiny Tiny RSS (M1 walking skeleton).
//!
//! Serves, under the configured base path (default `/tt-rss`):
//!   * static assets byte-for-byte from the doc root,
//!   * the login page (GET `/` and `/index.php` when not authenticated),
//!   * the login `POST` (`public.php?op=login`), including the TOTP challenge,
//!   * DB-backed encrypted sessions (read per request, persisted when changed),
//!   * the authenticated SPA shell (GET `/` and `/index.php`),
//!   * `/healthz`.

mod auth;
mod counters;
mod feeds;
mod headlines;
mod mime;
mod prefs;
mod render;
mod rpc;
mod sanitizer;
mod session;
mod time;

use axum::{
    body::Body,
    extract::{Extension, Form, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rand::Rng;
use serde::Deserialize;
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value;
use ttrss_core::session as core_session;
use tokio::fs;

/// Golden-DB schema version (used for `ref_schema_version` on login).
const SCHEMA_VERSION: i64 = 151;

#[derive(Clone)]
struct AppState {
    config: Config,
    db: Db,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let db = Db::connect(&config).await?;
    tracing::info!(
        listen = %config.listen,
        docroot = %config.docroot.display(),
        base = %config.base_path(),
        "starting tt-rust"
    );

    let state = AppState {
        config: config.clone(),
        db,
    };
    let app = build_router(&state).with_state(state);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(addr = %config.listen, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_router(state: &AppState) -> Router<AppState> {
    let base = state.config.base_path();

    let inner = Router::new()
        .route("/healthz", get(healthz))
        .route("/", get(index))
        .route("/index.php", get(index))
        .route("/public.php", get(public_handler).post(public_handler))
        .route("/backend.php", get(rpc::handler).post(rpc::handler))
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
    let html = render::render_shell(csrf, ts);
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
    match form.op.as_deref() {
        Some("login") => login_flow(&state, sess.0, &query, &form).await,
        Some("logout") => logout_flow(&state, sess.0, &form).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct PublicQuery {
    #[serde(rename = "return")]
    return_url: Option<String>,
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
            .body(Body::from(r#"{"error":{"code":"E_UNAUTHORIZED","params":[]}}"#))
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

/// PHP session `nocache` cache-limiter headers, emitted on responses whose
/// session was started (the default limiter for backend/public/API entry
/// points; the HTML shells opt out via `session_cache_limiter('')`).
fn nocache_headers(headers: &mut axum::http::HeaderMap) {
    use axum::http::HeaderValue;
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-cache, must-revalidate"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("Thu, 19 Nov 1981 08:52:00 GMT"));
}

/// `hash_equals`: constant-time string comparison (false on length mismatch).
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

/// `Handler_Public::login` + `UserHelper::authenticate` (web, auth_internal).
async fn login_flow(
    state: &AppState,
    sess: session::Sess,
    query: &PublicQuery,
    form: &PublicForm,
) -> Response {
    let login = form.login.as_deref().unwrap_or("").to_string();
    let password = form.password.as_deref().unwrap_or("").to_string();
    let otp = form.otp.as_deref().and_then(|s| s.trim().parse::<i64>().ok());
    let return_url = query.return_url.as_deref().unwrap_or("");
    let self_url = state.config.self_url_rtrim();

    match auth::authenticate(&state.db, &login, &password, otp).await {
        auth::AuthOutcome::Success { id, login, access_level, pwd_hash } => {
            let now = now_unix();
            let mut data = core_session::Session::new();
            data.set("uid", Value::Int(id as i64));
            data.set("last_login_update", Value::Int(now));
            data.set("ip_address", Value::Bytes(b"127.0.0.1".to_vec()));
            data.set("csrf_token", Value::Bytes(random_hex(16).into_bytes()));
            data.set("auth_module", Value::Bytes(b"auth_internal".to_vec()));
            data.set("name", Value::Bytes(login.into_bytes()));
            data.set("access_level", Value::Int(access_level as i64));
            data.set("pwd_hash", Value::Bytes(pwd_hash.into_bytes()));
            data.set("ref_schema_version", Value::Int(SCHEMA_VERSION));
            data.set("bw_limit", Value::Bool(truthy(form.bw_limit.as_deref())));
            data.set("safe_mode", Value::Bool(checkbox(form.safe_mode.as_deref())));
            let new_id = core_session::new_session_id();
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

fn with_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
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

fn random_hex(n_bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n_bytes)
        .map(|_| format!("{:02x}", rng.gen_range(0u8..=255)))
        .collect()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Catch-all static asset handler: serves a file from the doc root
/// byte-for-byte with the oracle's static-file headers — correct Content-Type
/// (from the nginx `mime.types`), `Cache-Control: no-cache`, and no
/// `Accept-Ranges` (nginx omits it on the gzip/chunked path real clients hit).
async fn static_handler(
    State(state): State<AppState>,
    req: Request,
) -> axum::response::Response {
    let base = state.config.base_path();
    let uri_path = req.uri().path();
    let rel = if base.is_empty() {
        uri_path.to_string()
    } else {
        uri_path.strip_prefix(base.as_str()).unwrap_or(uri_path).to_string()
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
