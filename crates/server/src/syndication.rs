//! `public.php?op=rss` — syndicated feed output (`Handler_Public::rss`).
//!
//! PHP validates a per-feed/category access key against `ttrss_access_keys`,
//! then generates an Atom (or JSON) feed from the matching feed/category
//! articles. The Atom body is rendered by tt-rss's home-grown `MiniTemplator`
//! from `templates/generated_feed.txt` (no RSS/Atom library).
//!
//! Increment A (this module's current scope): the access-key lookup, the 403
//! for unknown keys, and the `Unknown format:` guard. The Atom/JSON body
//! itself is increment B.

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;

use crate::{AppState, nocache_headers};

/// `op=rss` handler. `id` is the feed (or category) id, `key` the access key,
/// `format` the requested output format (defaults to `atom`).
pub async fn handle_rss(
    state: &AppState,
    id: Option<&str>,
    key: Option<&str>,
    format: Option<&str>,
) -> Response {
    let format = format.unwrap_or("atom");
    let res = match lookup_access_key(state, key, id).await {
        None => forbidden(),
        Some(owner_uid) => {
            if format != "atom" && format != "json" {
                unknown_format(format)
            } else {
                generate_feed(state, owner_uid, id, format).await
            }
        }
    };
    let mut res = res;
    // public.php starts a session (nocache cache-limiter) unconditionally.
    nocache_headers(res.headers_mut());
    res
}

/// `ttrss_access_keys` lookup: returns the `owner_uid` for a matching
/// `(access_key, feed_id)` pair, or `None`. `owner_uid` is INT4 → decoded as
/// `i32`, widened to `i64` at the boundary.
async fn lookup_access_key(state: &AppState, key: Option<&str>, id: Option<&str>) -> Option<i64> {
    let (Some(key), Some(id)) = (key, id) else {
        return None;
    };
    let uid: Option<i32> =
        sqlx::query_scalar("SELECT owner_uid FROM ttrss_access_keys WHERE access_key = $1 AND feed_id = $2")
            .bind(key)
            .bind(id)
            .fetch_optional(state.db.pool())
            .await
            .ok()
            .flatten();
    uid.map(i64::from)
}

/// `403 Forbidden` — unknown/missing access key.
fn forbidden() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "text/html; charset=UTF-8")
        .body(Body::empty())
        .expect("valid response")
}

/// `Unknown format: $format.` — the requested format is not `atom`/`json`.
fn unknown_format(format: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(format!("Unknown format: {format}.")))
        .expect("valid response")
}

/// Atom (or JSON) feed body. Increment B renders the real Atom feed; for now
/// return the empty Atom document so the routing/headers are in place.
async fn generate_feed(
    state: &AppState,
    owner_uid: i64,
    id: Option<&str>,
    format: &str,
) -> Response {
    let _ = (state, owner_uid, id, format);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
        .body(Body::empty())
        .expect("valid response")
}
