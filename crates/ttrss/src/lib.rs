//! Library portion of the tt-rss server crate: the DB-backed domain ports
//! (auth, session, prefs, counters, headlines, sanitizer, …) shared by the
//! `ttrss` binary and the test suites.
//!
//! The binary (`src/main.rs`) owns the HTTP wiring: `AppState` construction,
//! routing, the login/logout flows, and static file serving.

pub mod api;
pub mod article;
pub mod auth;
pub mod cats;
pub mod controls;
pub mod core;
pub mod counters;
pub mod digest;
pub mod diskcache;
pub mod favicons;
pub mod feeds;
pub mod filters;
pub mod forgotpass;
pub mod headlines;
pub mod hotkeys;
pub mod http;
pub mod labels;
pub mod mime;
pub mod opml;
pub mod otp;
pub mod pluginhost;
pub mod plugins;
pub mod pref_timezones;
pub mod pref_ui;
pub mod prefs;
pub mod render;
pub mod rpc;
pub mod rss_utils;
pub mod sanitizer;
pub mod scheduler;
#[cfg(test)]
mod sanitizer_parity;
pub mod search;
pub mod search_date;
pub mod session;
pub mod settings;
pub mod syndication;
pub mod system;
pub mod i18n;
pub mod time;
pub mod updates;
pub mod urlhelper;
pub mod users;
pub mod util;
use std::sync::Arc;

use crate::core::config::Config;
use crate::core::db::Db;
use crate::pluginhost::PluginHost;
/// Shared server state (`AppState`): configuration + DB pool + the startup
/// plugin registry (M7.1).
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub db: Db,
    /// The plugin registry, built once at startup ([`PluginHost::startup`]).
    pub plugins: Arc<PluginHost>,
    /// Per-process prefs cache (the PHP's `Prefs` singleton `$cache`): one
    /// `SELECT` per (uid, profile), then memory hits. Invalidated on prefs save.
    pub(crate) prefs_cache: Arc<std::sync::RwLock<std::collections::HashMap<(i64, Option<i32>), Vec<(String, Option<i32>, String)>>>>,
}

impl AppState {
    pub fn new(config: Config, db: Db) -> Self {
        Self {
            plugins: Arc::new(PluginHost::startup(&crate::pluginhost::all_plugins())),
            config,
            db,
            prefs_cache: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }
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
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(
        axum::http::header::PRAGMA,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        axum::http::header::EXPIRES,
        HeaderValue::from_static("Thu, 19 Nov 1981 08:52:00 GMT"),
    );
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
