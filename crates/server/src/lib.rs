//! Library portion of the tt-rss server crate: the DB-backed domain ports
//! (auth, session, prefs, counters, headlines, sanitizer, …) shared by the
//! `tt-rust` binary and the test suites.
//!
//! The binary (`src/main.rs`) owns the HTTP wiring: `AppState` construction,
//! routing, the login/logout flows, and static file serving.

pub mod article;
pub mod auth;
pub mod counters;
pub mod feeds;
pub mod headlines;
pub mod labels;
pub mod mime;
pub mod prefs;
pub mod render;
pub mod rpc;
pub mod sanitizer;
#[cfg(test)]
mod sanitizer_parity;
pub mod session;
pub mod time;
pub mod util;

use ttrss_core::config::Config;
use ttrss_core::db::Db;

/// Shared server state (`AppState`): configuration + DB pool.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub db: Db,
}

/// `with_trailing_slash` from `functions.php`: append a trailing slash unless
/// the string already ends with one (empty string → "/").
pub fn with_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}

/// PHP session `nocache` cache-limiter headers, emitted on responses whose
/// session was started (the default limiter for backend/public/API entry
/// points; the HTML shells opt out via `session_cache_limiter('')`).
pub fn nocache_headers(headers: &mut axum::http::HeaderMap) {
    use axum::http::HeaderValue;
    headers.insert(axum::http::header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-cache, must-revalidate"));
    headers.insert(axum::http::header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(axum::http::header::EXPIRES, HeaderValue::from_static("Thu, 19 Nov 1981 08:52:00 GMT"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `with_trailing_slash` — the `test_with_trailing_slash_*` cases from
    /// tt-rss/tests_functional/FunctionsTest.php, ported 1:1.
    #[test]
    fn with_trailing_slash_cases() {
        // test_with_trailing_slash_already_has_slash
        assert_eq!(with_trailing_slash("/path/"), "/path/");
        // test_with_trailing_slash_no_slash
        assert_eq!(with_trailing_slash("/path"), "/path/");
        // test_with_trailing_slash_root
        assert_eq!(with_trailing_slash("/"), "/");
        // test_with_trailing_slash_empty
        assert_eq!(with_trailing_slash(""), "/");
        // test_with_trailing_slash_double_slash
        assert_eq!(with_trailing_slash("/path//"), "/path//");
    }
}
