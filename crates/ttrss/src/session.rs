//! Per-request session handling + a port of the PHP `Sessions` class.
//!
//! Mirrors the tt-rss session lifecycle: read the `ttrss_sid` cookie, load the
//! (optionally encrypted) session from `ttrss_sessions`, expose a mutable
//! handle to handlers, and persist it back after the request if it changed.
//! Anonymous requests (no cookie) never touch the database.

use crate::core::db::Db;
use crate::i18n::__;

use crate::core::phpser::Value;
use crate::core::session::{self, Session};
use crate::core::user;
use crate::util::now_unix;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use sqlx::{PgPool, Row};
use std::sync::{Arc, Mutex};
use tracing;

use crate::AppState;

/// Session cookie name (PHP `SESSION_NAME`).
pub const SESSION_NAME: &str = "ttrss_sid";
/// `SESSION_COOKIE_LIFETIME` — used when re-stamping persistent cookies.
pub const COOKIE_LIFETIME: i64 = 86400;
/// `ttrss_sessions.expire` = now + this (PHP `session_expire`).
const STORE_EXPIRE: i64 = COOKIE_LIFETIME;

/// Per-request session handle. Cloned into the request extensions and handed to
/// handlers; the wrapped lock lets handlers mutate it and the middleware
/// persist the result.
#[derive(Clone)]
pub struct Sess {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    id: String,
    data: Session,
    dirty: bool,
    /// `session_destroy()` (PHP logout): the middleware deletes the stored
    /// row instead of persisting it, and the session is dead for this request.
    destroyed: bool,
    /// Per-request self URL (PHP `Config::get_self_url()`, web-SAPI branch):
    /// computed from this request's `Host` / `X-Forwarded-Proto` / URI by the
    /// session middleware, so two requests of the same session served through
    /// different frontends each see their own value. Request-local like `id`
    /// and `dirty` — never part of the persisted `data`.
    self_url: String,
}

impl Sess {
    /// An empty session with the given id (no stored row, no data) — the
    /// anonymous form the middleware builds before a `load`; tests use it to
    /// seed session data via [`Sess::replace`].
    pub fn new(id: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                id,
                data: Session::new(),
                dirty: false,
                destroyed: false,
                self_url: String::new(),
            })),
        }
    }

    /// The current session id ("" if the request had no session cookie).
    pub fn id(&self) -> String {
        self.inner.lock().unwrap().id.clone()
    }
    /// A snapshot of the current session data (for handlers to read).
    pub fn data(&self) -> Session {
        self.inner.lock().unwrap().data.clone()
    }
    /// Replace the id + data entirely (a fresh login), marking it dirty.
    pub fn replace(&self, id: String, data: Session) {
        let mut g = self.inner.lock().unwrap();
        g.id = id;
        g.data = data;
        g.dirty = true;
    }
    /// Mutate the session data in place, marking it dirty.
    pub fn with_mut(&self, f: impl FnOnce(&mut Session)) {
        let mut g = self.inner.lock().unwrap();
        f(&mut g.data);
        g.dirty = true;
    }
    /// `UserHelper::logout` / `session_destroy()`: kill the session. The
    /// stored row is deleted by the middleware (instead of being re-saved) and
    /// the data is cleared so later handlers see an anonymous session.
    pub fn destroy(&self) {
        let mut g = self.inner.lock().unwrap();
        g.data = Session::new();
        g.dirty = false;
        g.destroyed = true;
    }
    /// Whether this request carried a session cookie (PHP: the session was
    /// auto-started, and `extend_session` re-stamped the cookie).
    pub fn had_cookie(&self) -> bool {
        !self.inner.lock().unwrap().id.is_empty()
    }
    /// This request's self URL (see [`Inner::self_url`]). Stamped by the
    /// session middleware before the handler runs; `""` only for hand-built
    /// sessions that skipped the middleware (tests stamp it explicitly).
    pub fn self_url(&self) -> String {
        self.inner.lock().unwrap().self_url.clone()
    }
    /// Stamp the per-request self URL (tests: mirrors the middleware).
    pub fn set_self_url(&self, url: &str) {
        self.inner.lock().unwrap().self_url = url.to_string();
    }
}

/// Read the `ttrss_sid` value from the request's `Cookie` header.
pub fn cookie_sid(req: &Request) -> Option<String> {
    let raw = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some((k, v)) = part.trim().split_once('=')
            && k == SESSION_NAME
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

// ── PHP `Sessions` class port (raw, string-level handler) ────────────────

/// `Sessions::exists`.
pub async fn exists(pool: &PgPool, id: &str) -> bool {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    found.unwrap_or(0) > 0
}

/// The byte-level core of [`read`] (avoids a lossy UTF-8 round-trip for
/// binary session payloads).
///
/// `None` only on DB failure (PHP returns `false` there). A missing session
/// is auto-created with empty data (PHP `session_start` behavior) and `[]`
/// is returned. When `key` is set but the stored value is not a usable
/// encrypted envelope, PHP's documented fallback (return the raw base64-
/// decoded data) is matched; a valid envelope that fails to decrypt is fatal
/// in PHP (TypeError) but degrades gracefully to the same fallback here.
async fn read_bytes(pool: &PgPool, key: Option<&[u8; 32]>, id: &str) -> Option<Vec<u8>> {
    let stored: Option<String> = match sqlx::query_scalar(
        "SELECT data FROM ttrss_sessions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            // TODO(scope): session lookup DB errors are swallowed — the
            // reference 500s (per-request get_schema_version); revisit for
            // parity (the container HEALTHCHECK then also sees DB outages).
            tracing::warn!(
                error = %e,
                "session lookup DB errors are swallowed — the reference 500s (per-request get_schema_version); revisit for parity (the container HEALTHCHECK then also sees DB outages)"
            );
            return None;
        }
    };
    match stored {
        Some(stored) => {
            let inner = base64::engine::general_purpose::STANDARD
                .decode(stored.as_bytes())
                .unwrap_or_default();
            let data = match key {
                Some(k) => session::decrypt_envelope(&inner, k).unwrap_or_else(|_| inner.clone()),
                None => inner,
            };
            Some(data)
        }
        None => {
            // PHP: INSERT INTO ttrss_sessions (id, data, expire) VALUES (?, '', ?)
            let created =
                sqlx::query("INSERT INTO ttrss_sessions (id, data, expire) VALUES ($1, '', $2)")
                    .bind(id)
                    .bind(now_unix() + STORE_EXPIRE)
                    .execute(pool)
                    .await
                    .is_ok();
            created.then(Vec::new)
        }
    }
}

/// `Sessions::read` — the session data string for `id` (see [`read_bytes`]).
pub async fn read(pool: &PgPool, key: Option<&[u8; 32]>, id: &str) -> Option<String> {
    read_bytes(pool, key, id)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// `Sessions::write` — store `data` (base64-encoded, encrypted into the
/// `{algo,nonce,payload}` envelope when `key` is set), upserting with a fresh
/// expiry. PHP does SELECT-then-UPDATE-or-INSERT; the upsert has the same net
/// effect.
pub async fn write(pool: &PgPool, key: Option<&[u8; 32]>, id: &str, data: &str) -> bool {
    let stored = match key {
        Some(k) => match session::encrypt_envelope(data.as_bytes(), k) {
            Ok(inner) => base64::engine::general_purpose::STANDARD.encode(inner),
            Err(e) => {
                tracing::error!(error = %e, "failed to encrypt session data");
                return false;
            }
        },
        None => base64::engine::general_purpose::STANDARD.encode(data.as_bytes()),
    };
    upsert_stored(pool, id, &stored).await
}

/// The shared `INSERT … ON CONFLICT` upsert behind [`write`] and [`save`].
async fn upsert_stored(pool: &PgPool, id: &str, stored: &str) -> bool {
    sqlx::query(
        "INSERT INTO ttrss_sessions (id, data, expire) VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET data = EXCLUDED.data, expire = EXCLUDED.expire",
    )
    .bind(id)
    .bind(stored)
    .bind(now_unix() + STORE_EXPIRE)
    .execute(pool)
    .await
    .is_ok()
}

/// `Sessions::destroy` — DELETE (true even if no rows matched).
pub async fn destroy(pool: &PgPool, id: &str) -> bool {
    sqlx::query("DELETE FROM ttrss_sessions WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .is_ok()
}

/// `Sessions::validate_session`.
///
/// Single-user mode and guests (no `uid`, or `uid == 0` — PHP `!empty`) always
/// pass. A logged-in session is re-validated against `ttrss_users`: the user
/// must exist, not be disabled, and its `pwd_hash` must match the session's.
/// On failure `login_error_msg` is set on `sess` (PHP `$_SESSION`). PHP's
/// `HOOK_VALIDATE_SESSION` (Sessions.php:144): the chain runs after all the
/// default checks pass — the callback keeps the *last* hook's result (the
/// loop stops at the first `false`), and that result is the return value.
///
/// TODO(port): the M8 `update.php` CLI is not ported yet; when
/// `dbupdate` lands it must call this function before the
/// access-level check (upstream bdb1c47, GHSA-9f8c-5cv6-jv5x).
pub async fn validate_session(
    state: &crate::AppState,
    single_user: bool,
    sess: &mut Session,
) -> bool {
    let pool = state.db.pool();
    if single_user {
        return true;
    }
    let Some(uid) = sess.get_int("uid").filter(|&u| u != 0) else {
        return true; // guest
    };
    let row = sqlx::query("SELECT pwd_hash, access_level FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => {
            let pwd_hash = r.get::<String, _>(0);
            let access_level = r.get::<i32, _>(1);
            if pwd_hash != sess.get_str("pwd_hash").unwrap_or("") {
                sess.set(
                    "login_error_msg",
                    Value::Bytes(__("Session failed to validate (password changed)").into_bytes()),
                );
                return false;
            }
            if access_level as i64 != sess.get_int("access_level").unwrap_or(0) {
                sess.set(
                    "login_error_msg",
                    Value::Bytes(
                        __("Session failed to validate (access level changed)").into_bytes(),
                    ),
                );
                return false;
            }
            if access_level == user::ACCESS_LEVEL_DISABLED {
                sess.set(
                    "login_error_msg",
                    Value::Bytes(
                        __("Session failed to validate (account is disabled)").into_bytes(),
                    ),
                );
                return false;
            }
            // The plugin chain (defaults to `true` with no registered hooks).
            let data = sess.clone();
            let s = crate::session::Sess::new("validate-session".into());
            s.replace("validate-session".into(), data);
            let mut pctx = crate::pluginhost::dispatch::session_ctx(state, &s).await;
            let mut hook_result = true;
            pctx.run_hooks_callback::<bool, _, _>(
                crate::pluginhost::HOOK_VALIDATE_SESSION,
                |plugin, host| Box::pin(async move { plugin.hook_validate_session(host).await }),
                |result, _| {
                    hook_result = *result;
                    !result
                },
            )
            .await;
            hook_result
        }
        None => {
            sess.set(
                "login_error_msg",
                Value::Bytes(__("Session failed to validate (user not found)").into_bytes()),
            );
            false
        }
    }
}

// ── Typed middleware layer ─────────────────────────────────────────────────

/// Load a session from the DB by id (empty session if unreadable). Like PHP
/// `session_start`, a missing session is auto-created (via [`read_bytes`]).
pub async fn load(db: &Db, key: Option<[u8; 32]>, id: &str) -> Session {
    let Some(raw) = read_bytes(db.pool(), key.as_ref(), id).await else {
        return Session::new();
    };
    if raw.is_empty() {
        return Session::new();
    }
    session::decode_data(&raw)
        .map(Session::from_items)
        .unwrap_or_default()
}

/// Persist a session to the DB (upsert), matching PHP `Sessions::write`.
pub async fn save(db: &Db, key: Option<[u8; 32]>, id: &str, sess: &Session) -> anyhow::Result<()> {
    let data = session::to_stored(sess.items(), key.as_ref())?;
    upsert_stored(db.pool(), id, &data)
        .await
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("failed to persist session"))
}

/// Format a unix timestamp as an RFC 1123 date, e.g.
/// `Sat, 05 Sep 2026 05:38:51 GMT` (for the re-stamped cookie `expires`).
pub fn rfc1123(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .map(|dt| dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

/// Router layer: load the session into the request extensions and persist it
/// after the handler if it changed. Anonymous requests skip the DB entirely.
/// Applied in `main` via `middleware::from_fn_with_state(state, session_middleware)`.
pub async fn session_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let id = cookie_sid(&req).unwrap_or_default();
    let data = if id.is_empty() {
        Session::new()
    } else {
        load(&state.db, state.config.encryption_key, &id).await
    };
    // Per-request self URL (PHP `Config::get_self_url()`, web-SAPI branch):
    // the single stamp site — every route passes through here, so handlers
    // read `sess.self_url()` without threading a parameter.
    let self_url = state.config.get_self_url(req.headers(), req.uri().path());
    let sess = Sess {
        inner: Arc::new(Mutex::new(Inner {
            id,
            data,
            dirty: false,
            destroyed: false,
            self_url,
        })),
    };
    req.extensions_mut().insert(sess.clone());

    let mut res = next.run(req).await;

    // Rolling cookie re-stamp (PHP `extend_session`): authenticated sessions
    // refresh the `ttrss_sid` cookie on every response with a fresh lifetime.
    // Skipped when the handler already set the cookie (e.g. the login 302,
    // which emits the simple `path=/` cookie from session creation).
    if res.headers().get(header::SET_COOKIE).is_none() {
        let g = sess.inner.lock().unwrap();
        let authenticated = !g.id.is_empty() && g.data.is_logged_in();
        let sid = g.id.clone();
        drop(g);
        if authenticated {
            let expires = rfc1123(now_unix() + COOKIE_LIFETIME);
            let cookie = format!(
                "{}={sid}; expires={expires}; Max-Age={COOKIE_LIFETIME}; path=/",
                SESSION_NAME
            );
            res.headers_mut().append(
                header::SET_COOKIE,
                cookie.parse().expect("valid set-cookie header"),
            );
        }
    }

    let (destroyed, dirty, id) = {
        let g = sess.inner.lock().unwrap();
        (g.destroyed, g.dirty, g.id.clone())
    };
    if destroyed && !id.is_empty() {
        if let Err(e) = sqlx::query("DELETE FROM ttrss_sessions WHERE id = $1")
            .bind(&id)
            .execute(state.db.pool())
            .await
        {
            tracing::error!(error = %e, "failed to destroy session");
        }
    } else if dirty && !id.is_empty() {
        let data = sess.inner.lock().unwrap().data.clone();
        if let Err(e) = save(&state.db, state.config.encryption_key, &id, &data).await {
            tracing::error!(error = %e, "failed to persist session");
        }
    }
    res
}
