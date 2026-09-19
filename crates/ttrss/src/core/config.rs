//! tt-rss configuration for the Rust server.
//!
//! Mirrors the subset of `classes/Config.php` the server depends on. Upstream,
//! every `Config::get(Config::CONST)` is overridden by a `TTRSS_<CONST>`
//! environment variable (see `Config::_ENVVAR_PREFIX` and the `_DEFAULTS[]`
//! table); we read the same `TTRSS_*` variables with the same defaults. A few
//! extra, M1-only fields (listen address, doc root) live here too.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use url::Url;

/// `Config::SCHEMA_VERSION` — the latest known DB schema version.
pub const SCHEMA_VERSION: i64 = 151;

/// `Config::get_version()` — the tt-rss version string for this checkout (the
/// frozen reference's `Ym-commit`, e.g. `26.08-2f7d831f`). The PHP computes it
/// from git/CI at runtime; the port pins the frozen reference's value.
pub const VERSION: &str = "26.08-2f7d831f";

/// tt-rss configuration (the fields the M1 server needs).
#[derive(Debug, Clone)]
pub struct Config {
    // Database (Config.php DB_HOST / DB_PORT / DB_USER / DB_NAME / DB_PASS / DB_SSLMODE)
    pub db_host: String,
    pub db_port: u16,
    pub db_user: String,
    pub db_pass: String,
    pub db_name: String,
    pub db_sslmode: String,

    /// Max shared-`PgPool` connections (the D3 = b single-pool knob). The
    /// daemon's `Semaphore(N)` bounds concurrent workhorse jobs (D4 = 2), so a
    /// pool size comfortably above `N` + the web-server's concurrent queries
    /// avoids pool exhaustion. Default 10 (the pre-knob hardcoded value).
    pub db_max_connections: u32,

    /// Self URL (Config.php SELF_URL_PATH), e.g. `http://127.0.0.1:8280/tt-rss`.
    pub self_url: String,

    /// Decoded 32-byte `TTRSS_ENCRYPTION_KEY`, or `None` if unset (no session
    /// encryption, matching `Config::get(Config::ENCRYPTION_KEY)` gating).
    pub encryption_key: Option<[u8; 32]>,

    /// `Config::SINGLE_USER_MODE` (T_BOOL, default empty → false): skip
    /// per-session user validation in `Sessions::validate_session`.
    pub single_user_mode: bool,

    /// `Config::FORBID_PASSWORD_CHANGES` (T_BOOL, default false): when true,
    /// `Pref_Prefs::changepassword` is refused with "Access forbidden.".
    pub forbid_password_changes: bool,
    /// `Config::DISABLE_LOGIN_FORM` (T_BOOL, default false): when true, the
    /// login form and the public `forgotpass` (password-recovery) page are
    /// disabled (a 403 Forbidden).
    pub disable_login_form: bool,

    /// `Config::PLUGINS` (T_STRING): the comma-separated list of
    /// system-enabled plugins (the `TTRSS_PLUGINS` env var).
    pub plugins: String,

    // -- M1 server-specific (not part of upstream Config.php) -------------
    /// Where the Rust HTTP server listens.
    pub listen: SocketAddr,
    /// Root of the static/web tree to serve byte-for-byte.
    pub docroot: PathBuf,
    /// Directory holding fetched feed favicons (`<feed_id>` files) — the
    /// `feed-icons` DiskCache. Determines which feeds have an `icon` in the
    /// feed tree. (Upstream lives under the app cache dir; here it's a
    /// deployment artifact, overridable via `TTRSS_RUST_FEED_ICONS_DIR`.)
    pub feed_icon_dir: PathBuf,
    /// `Config::MAX_FAVICON_FILE_SIZE` (T_INT): the max size of an uploaded
    /// custom favicon, in bytes (default 1 MiB). Overridable via
    /// `TTRSS_RUST_MAX_FAVICON_FILE_SIZE`.
    pub max_favicon_file_size: usize,
    /// `Config::FORCE_ARTICLE_PURGE` (T_INT, default 0): when non-zero, article
    /// purging is forced to this many days (overriding per-feed / per-user settings).
    /// `Config::FORCE_ARTICLE_PURGE` (T_INT, default 0): when non-zero, article
    /// purging is forced to this many days (overriding per-feed / per-user settings).
    pub force_article_purge: i32,

    // -- M6c daemon / fetch-tuning (Config.php, T_INT) --------------------
    /// `Config::FEED_FETCH_TIMEOUT` (T_INT, default 45): the fetch timeout in
    /// seconds for a feed update.
    pub feed_fetch_timeout: u64,
    /// `Config::FEED_FETCH_NO_CACHE_TIMEOUT` (T_INT, default 15): the fetch
    /// timeout when `$no_cache` is set (a faster fail for on-demand updates).
    pub feed_fetch_no_cache_timeout: u64,
    /// `Config::MAX_CONDITIONAL_INTERVAL` (T_INT, default 3600*12 = 12h): once
    /// `last_unconditional` is older than this, force an unconditional fetch
    /// (no `If-Modified-Since`).
    pub max_conditional_interval: i64,
    /// `Config::HTTP_429_THROTTLE_INTERVAL` (T_INT, default 3600): the base
    /// throttle interval (seconds) for a 429 response; the actual delay is
    /// randomized to 1-2x this base.
    pub http_429_throttle_interval: i64,
    /// `Config::DAEMON_UNSUCCESSFUL_DAYS_LIMIT` (T_INT, default 30): auto-disable
    /// a feed (set `update_interval = -1`) when it has not succeeded for this
    /// many days.
    pub daemon_unsuccessful_days_limit: i64,
    /// `Config::RSS_EXTENDED_URL_FILTERING` (T_BOOL, default true): when true,
    /// feed URLs are extended-validated (the SSRF guard rejects loopback /
    /// private / reserved addresses). The daemon sets this from
    /// `$config['rss_extended_url_filtering']`.
    pub fetch_extended_url_filtering: bool,

    // -- M6c.3 daemon/scheduler (Config.php, T_INT) ------------------------
    /// `Config::DAEMON_SLEEP_INTERVAL` (T_INT, default 120): the daemon loop's
    /// sleep interval (seconds); the minimum is 60.
    pub daemon_sleep_interval: i64,
    /// `Config::DAEMON_MAX_JOBS` (T_INT, default 2): the max concurrent feed
    /// updates (the `Semaphore(N)` bound); the minimum is 1.
    pub daemon_max_jobs: i32,
    /// `Config::DAEMON_FEED_LIMIT` (T_INT, default 50): the max feeds to
    /// update per daemon cycle.
    pub daemon_feed_limit: i32,
    /// `Config::DAEMON_UPDATE_LOGIN_LIMIT` (T_INT, default 30): only update
    /// feeds owned by users whose last login is within this many days (0 or
    /// `SINGLE_USER_MODE` disables the check).
    pub daemon_update_login_limit: i32,

    // -- M6c.4 housekeeping / scheduled tasks (Config.php) ------------------
    /// `Config::CACHE_MAX_DAYS` (T_INT, default 7): feed-icon cache entries
    /// older than this are candidates for `cleanup_feed_icons` (a feed that
    /// still exists gets its mtime refreshed, otherwise the file is deleted).
    pub cache_max_days: i32,
    /// `Config::SCHEDULE_PURGE_ORPHANS` (cron, default `@daily`): the schedule
    /// for the `purge_orphans` task (delete `ttrss_entries` with no remaining
    /// `ttrss_user_entries`).
    pub schedule_purge_orphans: String,
    /// `Config::SCHEDULE_DISK_CACHE_EXPIRE_ALL` (cron, default `@daily`).
    pub schedule_disk_cache_expire_all: String,
    /// `Config::SCHEDULE_DISABLE_FAILED_FEEDS` (cron, default `@daily`): the
    /// schedule for the `disable_failed_feeds` task (auto-disable feeds that
    /// failed to update for `DAEMON_UNSUCCESSFUL_DAYS_LIMIT` days).
    pub schedule_disable_failed_feeds: String,
    /// `Config::SCHEDULE_CLEANUP_FEED_ICONS` (cron, default `@daily`).
    pub schedule_cleanup_feed_icons: String,
    /// `Config::SCHEDULE_LOG_DAEMON_UPDATE_LOGIN_LIMIT_USERS` (cron, default
    /// `@daily`): log the users excluded by the `DAEMON_UPDATE_LOGIN_LIMIT`
    /// check.
    pub schedule_log_daemon_update_login_limit_users: String,
    /// `Config::SCHEDULE_EXPIRE_ERROR_LOG` (cron, default `@hourly`).
    pub schedule_expire_error_log: String,
    /// `Config::SCHEDULE_EXPIRE_LOCK_FILES` (cron, default `@hourly`).
    pub schedule_expire_lock_files: String,
    /// `Config::SCHEDULE_SEND_HEADLINES_DIGESTS` (cron, default `@hourly`).
    pub schedule_send_headlines_digests: String,
}

impl Config {
    /// Build a [`Config`] from the environment (with upstream defaults).
    pub fn from_env() -> Result<Self> {
        fn env<T>(key: &str, default: &str) -> Result<T>
        where
            T: std::str::FromStr,
            T::Err: std::fmt::Display,
        {
            let raw = std::env::var(key).unwrap_or_else(|_| default.to_string());
            raw.parse().map_err(|e| anyhow!("env {key}={raw:?}: {e}"))
        }

        let db_user = std::env::var("TTRSS_DB_USER").unwrap_or_else(|_| "postgres".into());
        let db_name = std::env::var("TTRSS_DB_NAME").unwrap_or_else(|_| db_user.clone());

        Ok(Self {
            db_host: std::env::var("TTRSS_DB_HOST").unwrap_or_else(|_| "db".into()),
            db_port: env("TTRSS_DB_PORT", "5432")?,
            db_user,
            db_pass: std::env::var("TTRSS_DB_PASS").unwrap_or_default(),
            db_name,
            db_sslmode: std::env::var("TTRSS_DB_SSLMODE").unwrap_or_else(|_| "prefer".into()),
            db_max_connections: env("TTRSS_DB_MAX_CONNECTIONS", "10")?,
            self_url: std::env::var("TTRSS_SELF_URL_PATH")
                .unwrap_or_else(|_| "http://127.0.0.1:8280/tt-rss/".into()),
            encryption_key: match std::env::var("TTRSS_ENCRYPTION_KEY") {
                Ok(k) if !k.trim().is_empty() => Some(crate::core::crypt::hex_key(&k)?),
                _ => None,
            },
            single_user_mode: sql_bool_to_bool(
                &std::env::var("TTRSS_SINGLE_USER_MODE").unwrap_or_default(),
            ),
            forbid_password_changes: sql_bool_to_bool(
                &std::env::var("TTRSS_FORBID_PASSWORD_CHANGES").unwrap_or_default(),
            ),
            disable_login_form: sql_bool_to_bool(
                &std::env::var("TTRSS_DISABLE_LOGIN_FORM").unwrap_or_default(),
            ),
            plugins: std::env::var("TTRSS_PLUGINS").unwrap_or_else(|_| crate::pluginhost::default_plugins()),
            listen: env("TTRSS_RUST_LISTEN", "127.0.0.1:8281")?,
            docroot: std::env::var("TTRSS_RUST_DOCROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("tt-rss")),
            feed_icon_dir: std::env::var("TTRSS_RUST_FEED_ICONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("tt-rss/cache/feed-icons")),
            max_favicon_file_size: env("TTRSS_RUST_MAX_FAVICON_FILE_SIZE", "1048576")?,
            force_article_purge: env("TTRSS_RUST_FORCE_ARTICLE_PURGE", "0")?,
            feed_fetch_timeout: env("TTRSS_FEED_FETCH_TIMEOUT", "45")?,
            feed_fetch_no_cache_timeout: env("TTRSS_FEED_FETCH_NO_CACHE_TIMEOUT", "15")?,
            max_conditional_interval: env("TTRSS_MAX_CONDITIONAL_INTERVAL", "43200")?,
            http_429_throttle_interval: env("TTRSS_HTTP_429_THROTTLE_INTERVAL", "3600")?,
            daemon_unsuccessful_days_limit: env("TTRSS_DAEMON_UNSUCCESSFUL_DAYS_LIMIT", "30")?,
            fetch_extended_url_filtering: sql_bool_to_bool(
                &std::env::var("TTRSS_EXTENDED_URL_FILTERING").unwrap_or_default(),
            ),
            daemon_sleep_interval: env("TTRSS_DAEMON_SLEEP_INTERVAL", "120")?,
            daemon_max_jobs: env("TTRSS_DAEMON_MAX_JOBS", "2")?,
            daemon_feed_limit: env("TTRSS_DAEMON_FEED_LIMIT", "50")?,
            daemon_update_login_limit: env("TTRSS_DAEMON_UPDATE_LOGIN_LIMIT", "30")?,
            cache_max_days: env("TTRSS_CACHE_MAX_DAYS", "7")?,
            schedule_purge_orphans: std::env::var("TTRSS_SCHEDULE_PURGE_ORPHANS")
                .unwrap_or_else(|_| "@daily".into()),
            schedule_disk_cache_expire_all: std::env::var(
                "TTRSS_SCHEDULE_DISK_CACHE_EXPIRE_ALL",
            )
            .unwrap_or_else(|_| "@daily".into()),
            schedule_disable_failed_feeds: std::env::var("TTRSS_SCHEDULE_DISABLE_FAILED_FEEDS")
                .unwrap_or_else(|_| "@daily".into()),
            schedule_cleanup_feed_icons: std::env::var("TTRSS_SCHEDULE_CLEANUP_FEED_ICONS")
                .unwrap_or_else(|_| "@daily".into()),
            schedule_log_daemon_update_login_limit_users: std::env::var(
                "TTRSS_SCHEDULE_LOG_DAEMON_UPDATE_LOGIN_LIMIT_USERS",
            )
            .unwrap_or_else(|_| "@daily".into()),
            schedule_expire_error_log: std::env::var("TTRSS_SCHEDULE_EXPIRE_ERROR_LOG")
                .unwrap_or_else(|_| "@hourly".into()),
            schedule_expire_lock_files: std::env::var("TTRSS_SCHEDULE_EXPIRE_LOCK_FILES")
                .unwrap_or_else(|_| "@hourly".into()),
            schedule_send_headlines_digests: std::env::var(
                "TTRSS_SCHEDULE_SEND_HEADLINES_DIGESTS",
            )
            .unwrap_or_else(|_| "@hourly".into()),
        })
    }

    /// The `path` component of the self URL, with a single trailing slash
    /// stripped — e.g. `/tt-rss` (or `""` for a site-root deployment).
    pub fn base_path(&self) -> String {
        self.self_url
            .parse::<Url>()
            .map(|u| u.path().trim_end_matches('/').to_string())
            .unwrap_or_default()
    }

    /// The self URL without a trailing slash.
    pub fn self_url_rtrim(&self) -> String {
        self.self_url.trim_end_matches('/').to_string()
    }

    /// sqlx connection URL (equivalent of `Db::get_dsn()`).
    pub fn db_url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlenc(&self.db_user),
            urlenc(&self.db_pass),
            self.db_host,
            self.db_port,
            urlenc(&self.db_name),
            self.db_sslmode
        )
    }
}

/// PHP `sql_bool_to_bool()` (`include/functions.php`; the T_BOOL cast in
/// `Config::cast_to`): `$s && ($s !== "f" && $s !== "false")`. PHP string
/// truthiness applies ("" and "0" — and any numeric string equal to 0 — are
/// falsy), plus the legacy "f"/"false" strings.
pub fn sql_bool_to_bool(s: &str) -> bool {
    if s.is_empty() || s == "f" || s == "false" {
        return false;
    }
    if let Ok(f) = s.parse::<f64>() {
        return f != 0.0;
    }
    true
}

/// PHP `bool_to_sql_bool()` (`include/functions.php`): `bool` → `1`/`0` (INT4).
pub fn bool_to_sql_bool(s: bool) -> i32 {
    s as i32
}

/// PHP `checkbox_to_sql_bool()` (`include/functions.php`): `"on"` → `1`, else `0`.
pub fn checkbox_to_sql_bool(val: &str) -> i32 {
    if val == "on" {
        1
    } else {
        0
    }
}

/// Percent-encode a connection-string component (userinfo / dbname).
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(self_url: &str) -> Config {
        Config {
            db_host: "127.0.0.1".into(),
            db_port: 5432,
            db_user: "postgres".into(),
            db_pass: "password".into(),
            db_name: "postgres".into(),
            db_sslmode: "prefer".into(),
            db_max_connections: 10,
            self_url: self_url.into(),
            encryption_key: None,
            single_user_mode: false,
            forbid_password_changes: false,
            disable_login_form: false,
            plugins: "auth_internal".into(),
            listen: "127.0.0.1:8281".parse().unwrap(),
            docroot: PathBuf::from("tt-rss"),
            feed_icon_dir: PathBuf::from("tt-rss/cache/feed-icons"),
            max_favicon_file_size: 1024 * 1024,
            force_article_purge: 0,
            feed_fetch_timeout: 45,
            feed_fetch_no_cache_timeout: 15,
            max_conditional_interval: 43200,
            http_429_throttle_interval: 3600,
            daemon_unsuccessful_days_limit: 30,
            fetch_extended_url_filtering: true,
            daemon_sleep_interval: 120,
            daemon_max_jobs: 2,
            daemon_feed_limit: 50,
            daemon_update_login_limit: 30,
            cache_max_days: 7,
            schedule_purge_orphans: String::from("@daily"),
            schedule_disk_cache_expire_all: String::from("@daily"),
            schedule_disable_failed_feeds: String::from("@daily"),
            schedule_cleanup_feed_icons: String::from("@daily"),
            schedule_log_daemon_update_login_limit_users: String::from("@daily"),
            schedule_expire_error_log: String::from("@hourly"),
            schedule_expire_lock_files: String::from("@hourly"),
            schedule_send_headlines_digests: String::from("@hourly"),
        }
    }

    #[test]
    fn base_path_standard() {
        assert_eq!(cfg("http://127.0.0.1:8280/tt-rss/").base_path(), "/tt-rss");
        assert_eq!(cfg("http://127.0.0.1:8280/tt-rss").base_path(), "/tt-rss");
    }

    #[test]
    fn base_path_root() {
        assert_eq!(cfg("http://example.com/").base_path(), "");
    }

    #[test]
    fn self_url_rtrim() {
        assert_eq!(
            cfg("http://127.0.0.1:8280/tt-rss/").self_url_rtrim(),
            "http://127.0.0.1:8280/tt-rss"
        );
    }

    #[test]
    fn db_url_shape() {
        assert_eq!(
            cfg("http://x/tt-rss").db_url(),
            "postgres://postgres:password@127.0.0.1:5432/postgres?sslmode=prefer"
        );
    }

    #[test]
    fn urlenc_special_chars() {
        assert_eq!(urlenc("p@ss:word"), "p%40ss%3Aword");
        assert_eq!(urlenc("simple"), "simple");
    }

    #[test]
    fn sql_bool_to_bool_matches_php() {
        // PHP: $s && ($s !== "f" && $s !== "false") — "" and "0" are falsy.
        // test_sql_bool_to_bool_true_values (FunctionsTest.php)
        assert!(sql_bool_to_bool("t"));
        assert!(sql_bool_to_bool("true"));
        assert!(sql_bool_to_bool("1"));
        assert!(sql_bool_to_bool("y"));
        assert!(sql_bool_to_bool("anything"));
        // test_sql_bool_to_bool_false_values — NOTE: PHP's `null` input is out
        // of scope for a `&str` API; `""` is the empty/falsy stand-in.
        assert!(!sql_bool_to_bool("f"));
        assert!(!sql_bool_to_bool("false"));
        assert!(!sql_bool_to_bool("0"));
        assert!(!sql_bool_to_bool(""));
        assert!(!sql_bool_to_bool("0.0")); // numeric string == 0 is falsy
                                           // test_sql_bool_to_bool_uppercase_truthy — only exact lowercase
                                           // "f"/"false" are special.
        assert!(sql_bool_to_bool("F"));
        assert!(sql_bool_to_bool("False"));
    }

    #[test]
    fn sql_bool_conversions_match_php() {
        // test_bool_to_sql_bool_true / _false
        assert_eq!(bool_to_sql_bool(true), 1);
        assert_eq!(bool_to_sql_bool(false), 0);
        // test_checkbox_to_sql_bool_on / _other
        assert_eq!(checkbox_to_sql_bool("on"), 1);
        assert_eq!(checkbox_to_sql_bool("off"), 0);
    }
}
