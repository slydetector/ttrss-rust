//! HTML shell rendering (approach "b"): a template plus substitution of dynamic
//! fragments — the skeleton is authored once (mirroring `index.php` /
//! `login_form.php`) and only the server-injected values are computed. This is
//! not byte-for-byte HTML; whitespace and environment-dependent cache-bust
//! timestamps are normalized away by the harness.

use std::path::Path;

use ttrss_core::config::Config;

/// Max mtime (unix secs) of top-level `js/*.js` — mirrors
/// `get_scripts_timestamp()`.
pub fn scripts_timestamp(docroot: &Path) -> i64 {
    let mut max = 0i64;
    if let Ok(rd) = std::fs::read_dir(docroot.join("js")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("js") {
                max = max.max(mtime_secs(&p));
            }
        }
    }
    max
}

fn mtime_secs(p: &Path) -> i64 {
    p.metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `get_theme_path($theme, $default)`: resolve the theme CSS path and append its
/// mtime as a cache-buster.
pub fn theme_path(docroot: &Path, theme: &str, default: &str) -> String {
    for d in ["themes", "themes.local"] {
        let f = format!("{d}/{theme}");
        let p = docroot.join(&f);
        if p.is_file() {
            return format!("{f}?{}", mtime_secs(&p));
        }
    }
    let p = docroot.join(default);
    if p.is_file() {
        format!("{default}?{}", mtime_secs(&p))
    } else {
        default.to_string()
    }
}

/// PHP `urlencode()`: space → `+`, unreserved characters kept, else `%XX`.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

const LOGIN_TEMPLATE: &str = include_str!("../templates/login.html.tpl");
const OTP_TEMPLATE: &str = include_str!("../templates/otp_challenge.html.tpl");

/// Minimal HTML-escape for attribute values (PHP escapes these in `hidden_tag`).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Form fields to carry into the OTP challenge hidden inputs.
pub struct OtpForm {
    pub login: String,
    pub password: String,
    pub bw_limit: String,
    pub safe_mode: String,
    pub remember_me: String,
    pub profile: String,
}

/// Render the OTP challenge page (`auth_internal`: the user has OTP enabled and
/// no code was supplied yet). `return_url` is the already-encoded
/// `urlencode(with_trailing_slash($return))` used in the form action.
pub fn render_otp_challenge(config: &Config, return_url: &str, form: &OtpForm) -> String {
    let scripts_ts = scripts_timestamp(config.docroot.as_path());
    OTP_TEMPLATE
        .replace("__SCRIPTS_TS__", &scripts_ts.to_string())
        .replace("__RETURN_URL__", return_url)
        .replace("__LOGIN__", &html_escape(&form.login))
        .replace("__PASSWORD__", &html_escape(&form.password))
        .replace("__BW_LIMIT__", &html_escape(&form.bw_limit))
        .replace("__SAFE_MODE__", &html_escape(&form.safe_mode))
        .replace("__REMEMBER_ME__", &html_escape(&form.remember_me))
        .replace("__PROFILE__", &html_escape(&form.profile))
}

/// Render the not-authenticated login page.
pub fn render_login_page(config: &Config) -> String {
    let doc = config.docroot.as_path();
    let scripts_ts = scripts_timestamp(doc);
    let theme_light = theme_path(doc, "light.css", "themes/light.css");
    let theme_dark = theme_path(doc, "night.css", "themes/night.css");
    let return_url = urlencode(&format!("{}/", config.self_url_rtrim()));
    let lifetime = 86400; // SESSION_COOKIE_LIFETIME default

    LOGIN_TEMPLATE
        .replace("__SCRIPTS_TS__", &scripts_ts.to_string())
        .replace("__THEME_LIGHT__", &theme_light)
        .replace("__THEME_DARK__", &theme_dark)
        .replace("__RETURN_URL__", &return_url)
        .replace("__LIFETIME__", &lifetime.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        // Anchor the doc root at the real tt-rss checkout (two levels up from
        // this crate).
        let docroot =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tt-rss");
        Config {
            db_host: "127.0.0.1".into(),
            db_port: 5432,
            db_user: "postgres".into(),
            db_pass: "password".into(),
            db_name: "postgres".into(),
            db_sslmode: "prefer".into(),
            self_url: "http://127.0.0.1:8280/tt-rss/".into(),
            encryption_key: None,
            listen: "127.0.0.1:8281".parse().unwrap(),
            docroot,
        }
    }

    #[test]
    fn urlencode_matches_php() {
        assert_eq!(
            urlencode("http://127.0.0.1:8280/tt-rss/"),
            "http%3A%2F%2F127.0.0.1%3A8280%2Ftt-rss%2F"
        );
    }

    #[test]
    fn login_page_substitutes_dynamic_values() {
        let html = render_login_page(&test_config());
        // No placeholders remain.
        assert!(!html.contains("__SCRIPTS_TS__"));
        assert!(!html.contains("__RETURN_URL__"));
        assert!(html.contains("return=http%3A%2F%2F127.0.0.1%3A8280%2Ftt-rss%2F"));
        assert!(html.contains("const __default_light_theme = \"themes/light.css?"));
        assert!(html.contains("src=\"js/common.js?"));
        // Scripts timestamp resolves to a real (nonzero) mtime from the docroot.
        let cfg2 = test_config();
        assert!(scripts_timestamp(cfg2.docroot.as_path()) > 0);
    }
}
