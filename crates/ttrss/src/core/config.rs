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

/// `Config::PLUGINS` (T_STRING, default "auth_internal"): the built-in
/// `auth_internal` auth module is the only *system* plugin enabled by default;
/// the rest are enabled per-user (`_ENABLED_PLUGINS`). Single source of truth
/// for the `TTRSS_PLUGINS` default (replaces `pluginhost::default_plugins()`).
pub const DEFAULT_PLUGINS: &str = "auth_internal";

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
    /// `Config::AUTH_AUTO_CREATE` (T_BOOL, default "true"): auto-create users
    /// on successful external authentication (the `auth_remote` plugin's
    /// `auto_create_user` + the proxy-header name/email updates).
    pub auth_auto_create: bool,
    /// The `auth_remote` plugin's `AUTH_REMOTE_POST_LOGOUT_URL` (T_STRING,
    /// default ""): the URL the logout handler redirects to (the
    /// `HOOK_POST_LOGOUT` chain's target) when set (`.env`
    /// `TTRSS_AUTH_REMOTE_POST_LOGOUT_URL`, as in the reference's comment).
    // TODO(soc): plugin-scoped knob (PHP owns the const in the `auth_remote`
    // plugin, read via `Config::get(self::AUTH_REMOTE_POST_LOGOUT_URL)`),
    // hoisted into the global Config. Move to a dedicated plugin-config layer
    // when a second plugin knob motivates it (tracked in docs/01-plan.md).
    pub auth_remote_post_logout_url: String,

    /// `Config::PLUGINS` (T_STRING): the comma-separated list of
    /// system-enabled plugins (the `TTRSS_PLUGINS` env var).
    pub plugins: String,

    // -- M1 server-specific (not part of upstream Config.php) -------------
    /// Where the Rust HTTP server listens.
    pub listen: SocketAddr,
    /// Root of the static/web tree to serve byte-for-byte.
    pub docroot: PathBuf,
    /// The disk-cache root (PHP `Config::CACHE_DIR`, default `"cache"`): a
    /// writable sibling of the (read-only/static) docroot, CWD-relative. All
    /// disk caches are fixed subdirs under it — `feed-icons` (favicons) and
    /// `images` (downloaded media). Overridable via `TTRSS_CACHE_DIR`.
    pub cache_dir: PathBuf,
    /// `Config::MAX_FAVICON_FILE_SIZE` (T_INT): the max size of an uploaded
    /// custom favicon, in bytes (default 1 MiB). Overridable via
    /// `TTRSS_MAX_FAVICON_FILE_SIZE`.
    pub max_favicon_file_size: usize,
    /// `Config::MAX_CACHE_FILE_SIZE` (T_INT, default 64 MiB): the maximum bytes
    /// a single media file may occupy when downloaded to the cache.
    pub max_cache_file_size: usize,
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
    /// `Config::SCHEDULE_SEND_HEADLINES_DIGESTS` (cron, default `@hourly`).
    pub schedule_send_headlines_digests: String,
}

/// The spec-faithful defaults (the `from_env` values, matching the frozen PHP
/// `_DEFAULTS`). Tests override only the fields that differ (DB, `listen`, …)
/// via `Config { .., ..Default::default() }` — see review §4.4.
impl Default for Config {
    fn default() -> Self {
        Self {
            db_host: "db".into(),
            db_port: 5432,
            db_user: "postgres".into(),
            db_pass: String::new(),
            db_name: "postgres".into(),
            db_sslmode: "prefer".into(),
            self_url: "https://example.com/tt-rss".into(),
            encryption_key: None,
            single_user_mode: false,
            forbid_password_changes: false,
            disable_login_form: false,
            auth_auto_create: true,
            auth_remote_post_logout_url: String::new(),
            plugins: DEFAULT_PLUGINS.to_string(),
            listen: "127.0.0.1:8280".parse().unwrap(),
            docroot: PathBuf::from("tt-rss"),
            cache_dir: PathBuf::from("cache"),
            max_favicon_file_size: 1048576,
            max_cache_file_size: 67108864,
            force_article_purge: 0,
            feed_fetch_timeout: 45,
            feed_fetch_no_cache_timeout: 15,
            max_conditional_interval: 43200,
            http_429_throttle_interval: 3600,
            daemon_unsuccessful_days_limit: 30,
            daemon_sleep_interval: 120,
            daemon_max_jobs: 2,
            daemon_feed_limit: 50,
            daemon_update_login_limit: 30,
            cache_max_days: 7,
            schedule_purge_orphans: "@daily".into(),
            schedule_disk_cache_expire_all: "@daily".into(),
            schedule_disable_failed_feeds: "@daily".into(),
            schedule_cleanup_feed_icons: "@daily".into(),
            schedule_log_daemon_update_login_limit_users: "@daily".into(),
            schedule_expire_error_log: "@hourly".into(),
            schedule_send_headlines_digests: "@hourly".into(),
        }
    }
}

impl Config {
    /// The feed-favicon cache dir (`<cache_dir>/feed-icons`, PHP
    /// `DiskCache::instance("feed-icons")`).
    pub fn feed_icon_dir(&self) -> PathBuf {
        self.cache_dir.join("feed-icons")
    }
    /// The downloaded-media cache dir (`<cache_dir>/images`, PHP
    /// `DiskCache::instance("images")`).
    pub fn images_dir(&self) -> PathBuf {
        self.cache_dir.join("images")
    }
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
            auth_auto_create: sql_bool_to_bool(
                &std::env::var("TTRSS_AUTH_AUTO_CREATE").unwrap_or_else(|_| "true".into()),
            ),
            auth_remote_post_logout_url:
                std::env::var("TTRSS_AUTH_REMOTE_POST_LOGOUT_URL").unwrap_or_default(),
            plugins: std::env::var("TTRSS_PLUGINS").unwrap_or_else(|_| DEFAULT_PLUGINS.to_string()),
            listen: env("TTRSS_LISTEN_PORT", "127.0.0.1:8280")?,
            docroot: std::env::var("TTRSS_DOCROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("tt-rss")),
            cache_dir: std::env::var("TTRSS_CACHE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("cache")),
            max_favicon_file_size: env("TTRSS_MAX_FAVICON_FILE_SIZE", "1048576")?,
            max_cache_file_size: env("TTRSS_MAX_CACHE_FILE_SIZE", "67108864")?,
            force_article_purge: env("TTRSS_FORCE_ARTICLE_PURGE", "0")?,
            feed_fetch_timeout: env("TTRSS_FEED_FETCH_TIMEOUT", "45")?,
            feed_fetch_no_cache_timeout: env("TTRSS_FEED_FETCH_NO_CACHE_TIMEOUT", "15")?,
            max_conditional_interval: env("TTRSS_MAX_CONDITIONAL_INTERVAL", "43200")?,
            http_429_throttle_interval: env("TTRSS_HTTP_429_THROTTLE_INTERVAL", "3600")?,
            daemon_unsuccessful_days_limit: env("TTRSS_DAEMON_UNSUCCESSFUL_DAYS_LIMIT", "30")?,
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

    /// A minimal config for unit tests (no DB, no plugins).
    pub fn for_test() -> Self {
        Self {
            db_host: "127.0.0.1".into(),
            db_pass: "postgres".into(),
            db_sslmode: "disable".into(),
            self_url: "http://127.0.0.1:8280/tt-rss/".into(),
            plugins: String::new(),
            listen: "127.0.0.1:8281".parse().unwrap(),
            cache_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
            daemon_sleep_interval: 60,
            schedule_log_daemon_update_login_limit_users: String::from("@hourly"),
            ..Default::default()
        }
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
            db_pass: "password".into(),
            self_url: self_url.into(),
            listen: "127.0.0.1:8281".parse().unwrap(),
            cache_dir: PathBuf::from("tt-rss/cache/feed-icons"),
            ..Default::default()
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
