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
            raw.parse()
                .map_err(|e| anyhow!("env {key}={raw:?}: {e}"))
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
                Ok(k) if !k.trim().is_empty() => Some(crate::crypt::hex_key(&k)?),
                _ => None,
            },
            listen: env("TTRSS_RUST_LISTEN", "127.0.0.1:8281")?,
            docroot: std::env::var("TTRSS_RUST_DOCROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("tt-rss")),
            feed_icon_dir: std::env::var("TTRSS_RUST_FEED_ICONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("tt-rss/cache/feed-icons")),
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
            self_url: self_url.into(),
            encryption_key: None,
            listen: "127.0.0.1:8281".parse().unwrap(),
            docroot: PathBuf::from("tt-rss"),
            feed_icon_dir: PathBuf::from("tt-rss/cache/feed-icons"),
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
}
