//! Per-request session handling.
//!
//! Mirrors the tt-rss session lifecycle: read the `ttrss_sid` cookie, load the
//! (optionally encrypted) session from `ttrss_sessions`, expose a mutable
//! handle to handlers, and persist it back after the request if it changed.
//! Anonymous requests (no cookie) never touch the database.

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::{Arc, Mutex};
use sqlx::Row;
use ttrss_core::db::Db;
use ttrss_core::session::{self, Session};

use crate::AppState;

/// Session cookie name (PHP `SESSION_NAME`).
pub const SESSION_NAME: &str = "ttrss_sid";
/// `SESSION_COOKIE_LIFETIME` — used when re-stamping persistent cookies.
pub const COOKIE_LIFETIME: i64 = 86400;
/// `ttrss_sessions.expire` = now + this (PHP `session_expire`).
const STORE_EXPIRE: i64 = 86400;

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
}

impl Sess {
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
    /// Mutate the session data in place, marking it dirty. (Used by the
    /// backend/prefs handlers in M2+; kept now so the `Sess` API is stable.)
    #[allow(dead_code)]
    pub fn with_mut(&self, f: impl FnOnce(&mut Session)) {
        let mut g = self.inner.lock().unwrap();
        f(&mut g.data);
        g.dirty = true;
    }
}

/// Read the `ttrss_sid` value from the request's `Cookie` header.
pub fn cookie_sid(req: &Request) -> Option<String> {
    let raw = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some((k, v)) = part.trim().split_once('=') {
            if k == SESSION_NAME {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Load a session from the DB by id (empty session if absent/unreadable).
pub async fn load(db: &Db, key: Option<[u8; 32]>, id: &str) -> Session {
    let row = sqlx::query("SELECT data FROM ttrss_sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => {
            let stored = r.get::<String, _>(0);
            session::from_stored(&stored, key.as_ref()).unwrap_or_default()
        }
        None => Session::new(),
    }
}

/// Persist a session to the DB (upsert), matching PHP `Sessions::write`.
pub async fn save(db: &Db, key: Option<[u8; 32]>, id: &str, sess: &Session) -> anyhow::Result<()> {
    let data = session::to_stored(sess.items(), key.as_ref())?;
    let expire = now_unix() + STORE_EXPIRE;
    sqlx::query(
        "INSERT INTO ttrss_sessions (id, data, expire) VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET data = EXCLUDED.data, expire = EXCLUDED.expire",
    )
    .bind(id)
    .bind(&data)
    .bind(expire)
    .execute(db.pool())
    .await?;
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a unix timestamp as an RFC 1123 date, e.g.
/// `Sat, 05 Sep 2026 05:38:51 GMT` (for the re-stamped cookie `expires`).
fn rfc1123(unix_secs: i64) -> String {
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
    let sess = Sess {
        inner: Arc::new(Mutex::new(Inner { id, data, dirty: false })),
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

    if sess.inner.lock().unwrap().dirty {
        let (id, data) = {
            let g = sess.inner.lock().unwrap();
            (g.id.clone(), g.data.clone())
        };
        if let Err(e) = save(&state.db, state.config.encryption_key, &id, &data).await {
            tracing::error!(error = %e, "failed to persist session");
        }
    }
    res
}
