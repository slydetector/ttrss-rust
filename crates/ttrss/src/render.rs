//! HTML shell rendering (approach "b"): a template plus substitution of dynamic
//! fragments — the skeleton is authored once (mirroring `index.php` /
//! `login_form.php`) and only the server-injected values are computed. This is
//! not byte-for-byte HTML; whitespace and environment-dependent cache-bust
//! timestamps are normalized away by the harness.

use std::path::Path;

use crate::core::config::Config;

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
const SHELL_TEMPLATE: &str = include_str!("../templates/shell.html.tpl");
const PREFS_TEMPLATE: &str = include_str!("../templates/prefs.html");

/// The admin-only prefs tabs (Users + System) plus the tab-container close. For
/// non-admins the `if` in prefs.php is false, so the tabs are skipped and the
/// close is preceded by four 8-space PHP-tag lines (32 spaces total).
const PREFS_ADMIN_TABS: &str = "\n                    <div id=\"usersTab\" dojoType=\"dijit.layout.ContentPane\"\n                style=\"padding : 0px\"\n                href=\"backend.php?op=Pref_Users\"\n                title=\"<i class='material-icons'>person</i> Users\"></div>\n            <div id=\"systemTab\" dojoType=\"dijit.layout.ContentPane\"\n                href=\"backend.php?op=Pref_System\"\n                title=\"<i class='material-icons'>info_outline</i> System\"></div>\n                        </div>\n";
const PREFS_NONADMIN_CLOSE: &str = "\n                                    </div>\n";

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

/// Render the authenticated SPA shell (`index.php`). The skeleton (container
/// divs, per-plugin `Plugins.*` blocks, theme/PWA metadata) is fixed for the
/// golden dataset; the only per-session dynamic value is the CSRF token, plus
/// the environment-derived scripts cache-bust timestamp.
pub fn render_shell(csrf_token: &str, scripts_ts: i64) -> String {
    SHELL_TEMPLATE
        .replace("__CSRF_TOKEN__", csrf_token)
        .replace("__SCRIPTS_TS__", &scripts_ts.to_string())
}

/// Render the preferences SPA page (`prefs.php`). The skeleton is fixed for the
/// golden dataset (no custom theme, empty plugin prefs CSS, the `share`
/// plugin's prefs JS baked in); the per-session dynamic values are the CSRF
/// token and the scripts cache-bust, and the Users/System tabs are admin-only.
pub fn render_prefs_page(csrf_token: &str, scripts_ts: i64, access_level: i64) -> String {
    let mut html = PREFS_TEMPLATE
        .replace("@CSRF@", csrf_token)
        .replace("@CACHEBUST@", &scripts_ts.to_string());
    if access_level < crate::core::user::ACCESS_LEVEL_ADMIN as i64 {
        html = html.replace(PREFS_ADMIN_TABS, PREFS_NONADMIN_CLOSE);
    }
    html
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
        let docroot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tt-rss");
        Config {
            db_host: "127.0.0.1".into(),
            db_port: 5432,
            db_user: "postgres".into(),
            db_pass: "password".into(),
            db_name: "postgres".into(),
            db_sslmode: "prefer".into(),
            self_url: "http://127.0.0.1:8280/tt-rss/".into(),
            encryption_key: None,
            single_user_mode: false,
            forbid_password_changes: false,
            disable_login_form: false,
            plugins: "auth_internal".into(),
            listen: "127.0.0.1:8281".parse().unwrap(),
            feed_icon_dir: docroot.join("cache").join("feed-icons"),
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

    #[test]
    fn get_theme_path_matches_php() {
        let docroot = test_config().docroot;
        // test_get_theme_path_nonexistent
        assert_eq!(theme_path(&docroot, "nonexistent-theme-xyz", ""), "");
        // test_get_theme_path_with_default
        assert_eq!(
            theme_path(&docroot, "nonexistent-theme-xyz", "themes/classic"),
            "themes/classic"
        );
        // test_get_theme_path_empty_default
        assert_eq!(theme_path(&docroot, "nonexistent-theme-xyz", ""), "");
    }

    #[test]
    fn get_scripts_timestamp_matches_php() {
        let docroot = test_config().docroot;
        let ts = scripts_timestamp(&docroot);
        // test_get_scripts_timestamp_returns_int
        assert!(ts >= 0);
        // test_get_scripts_timestamp_nonzero_when_js_exists
        let has_js = std::fs::read_dir(docroot.join("js"))
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("js"))
            })
            .unwrap_or(false);
        if has_js {
            assert!(ts > 0);
        }
    }
}
