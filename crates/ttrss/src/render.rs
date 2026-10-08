//! HTML shell rendering (approach "b"): a template plus substitution of dynamic
//! fragments — the skeleton is authored once (mirroring `index.php` /
//! `login_form.php`) and only the server-injected values are computed. This is
//! not byte-for-byte HTML; whitespace and environment-dependent cache-bust
//! timestamps are normalized away by the harness.

use std::path::Path;

use crate::core::config::Config;
use crate::i18n::__;

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
/// mtime as a cache-buster. User themes live in the data dir's `themes.local/`
/// (docs/19 volume model); the URL keeps the docroot-relative shape
/// `themes.local/<t>?<mtime>` — the static handler resolves that prefix from
/// the data dir.
pub fn theme_path(docroot: &Path, data_dir: &Path, theme: &str, default: &str) -> String {
    for (dir, url_prefix) in [(docroot, "themes"), (data_dir, "themes.local")] {
        let f = format!("{url_prefix}/{theme}");
        let p = dir.join(url_prefix).join(theme);
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
    let mut env = minijinja::Environment::new();
    env.add_function("__", |msg: String| -> String {
        __(&msg)
    });
    let template = env
        .template_from_named_str("otp_challenge.html.tpl", OTP_TEMPLATE)
        .unwrap();
    template
        .render(minijinja::context! {
            scripts_ts,
            return_url,
            login => html_escape(&form.login),
            password => html_escape(&form.password),
            bw_limit => html_escape(&form.bw_limit),
            safe_mode => html_escape(&form.safe_mode),
            remember_me => html_escape(&form.remember_me),
            profile => html_escape(&form.profile),
        })
        .unwrap_or_default()
}

/// `stylesheet_tag(get_theme_path($theme), ['id' => 'theme_css'])` — the shell's
/// per-user theme `<link>` (PHP `index.php`/`prefs.php` head). Emits nothing when
/// the pref is empty or the theme file doesn't exist (`theme_exists`); the
/// resolved path carries its mtime cache-buster via [`theme_path`].
pub fn theme_css_link(docroot: &Path, data_dir: &Path, theme: &str) -> String {
    if theme.is_empty() {
        return String::new();
    }
    let exists = docroot.join("themes").join(theme).exists()
        || data_dir.join("themes.local").join(theme).exists();
    if !exists {
        return String::new();
    }
    let path = theme_path(docroot, data_dir, theme, "");
    format!(
        "<link href=\"{path}\" rel=\"stylesheet\" type=\"text/css\" data-orig-href=\"{path}\" id=\"theme_css\"/>"
    )
}

/// Render the authenticated SPA shell (`index.php`). The skeleton (container
/// divs, per-plugin `Plugins.*` blocks, theme/PWA metadata) is fixed for the
/// golden dataset; the per-session dynamic values are the CSRF token, the
/// scripts cache-bust timestamp, the per-user theme link, the active
/// plugins' shell assets and the `HOOK_MAIN_TOOLBAR_BUTTON` button row (PHP
/// `index.php:195`, rendered between the order-0 and order-5 toolbar markers).
#[allow(clippy::too_many_arguments)] // mirrors PHP `index.php` shell rendering
pub fn render_shell(
    csrf_token: &str,
    scripts_ts: i64,
    theme_css_link: &str,
    user_css_style: &str,
    plugin_css: &str,
    plugin_js: &str,
    main_toolbar_buttons: &str,
    feed_tree_hooks: &str,
    toolbar_buttons_hooks: &str,
    action_item_hooks: &str,
    hide_logout: bool,
) -> String {
    let mut env = minijinja::Environment::new();
    env.add_function("__", |msg: String| -> String {
        __(&msg)
    });
    env.add_filter("tojson", |v: minijinja::Value| -> minijinja::Value {
        let s = v.to_string();
        minijinja::Value::from(serde_json::to_string(&s).unwrap())
    });
    let template = env
        .template_from_named_str("shell.html.tpl", SHELL_TEMPLATE)
        .unwrap();
    template
        .render(minijinja::context! {
            csrf_token => csrf_token,
            scripts_ts => scripts_ts,
            theme_css_link => theme_css_link,
            user_css_style => user_css_style,
            plugin_css => plugin_css,
            plugin_js => plugin_js,
            main_toolbar_buttons => main_toolbar_buttons,
            feed_tree_hooks => feed_tree_hooks,
            toolbar_buttons_hooks => toolbar_buttons_hooks,
            action_item_hooks => action_item_hooks,
            hide_logout => hide_logout,
        })
        .unwrap()
}
/// Render the preferences SPA page (`prefs.php`). The per-session dynamic
/// values are the CSRF token, the scripts cache-bust, the active plugins'
/// prefs assets (PHP `prefs.php`: `get_prefs_css()` in the `<style>` block,
/// each `get_prefs_js()` in a named try/catch), and the Users/System tabs
/// (admin-only).
// #[allow]: the 8 params are the template's substitution slots, mirroring the
// PHP page's inputs 1:1; a param struct would hide the parity surface.
#[allow(clippy::too_many_arguments)]
pub fn render_prefs_page(
    csrf_token: &str,
    scripts_ts: i64,
    access_level: i64,
    theme_css_link: &str,
    user_css_style: &str,
    plugin_js: &str,
    plugin_css: &str,
    prefs_tabs_hooks: &str,
) -> String {
    let mut html = PREFS_TEMPLATE
        .replace("@CSRF@", csrf_token)
        .replace("@CACHEBUST@", &scripts_ts.to_string())
        .replace("__THEME_CSS_LINK__", theme_css_link)
        .replace("__USER_CSS_STYLE__", user_css_style)
        .replace("__PREFS_PLUGIN_CSS__", plugin_css)
        .replace("__PREFS_PLUGIN_JS__", plugin_js)
        .replace("__PREFS_TABS_HOOKS__", prefs_tabs_hooks);
    if access_level < crate::core::user::ACCESS_LEVEL_ADMIN as i64 {
        html = html.replace(PREFS_ADMIN_TABS, PREFS_NONADMIN_CLOSE);
    }
    html
}

/// Render the not-authenticated login page. `self_url` is the per-request
/// detected self URL (PHP `Config::get_self_url`), used for the form's
/// `returnUrl`.
pub fn render_login_page(config: &Config, additional_buttons: &str, self_url: &str) -> String {
    let doc = config.docroot.as_path();
    let scripts_ts = scripts_timestamp(doc);
    let theme_light = theme_path(doc, &config.data_dir, "light.css", "themes/light.css");
    let theme_dark = theme_path(doc, &config.data_dir, "night.css", "themes/night.css");
    let return_url = urlencode(&format!("{}/", self_url));
    let lifetime = 86400; // SESSION_COOKIE_LIFETIME default
    let mut env = minijinja::Environment::new();
    env.add_function("__", |msg: String| -> String {
        __(&msg)
    });
    let template = env
        .template_from_named_str("login.html.tpl", include_str!("../templates/login.html.tpl"))
        .unwrap();
    template
        .render(minijinja::context! {
            scripts_ts,
            theme_light,
            theme_dark,
            return_url,
            lifetime,
            additional_buttons,
        })
        .unwrap_or_default()
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
            db_pass: "password".into(),
            self_url: "http://127.0.0.1:8280/tt-rss/".into(),
            cache_dir: docroot.join("cache"),
            listen: "127.0.0.1:8281".parse().unwrap(),
            docroot,
            ..Default::default()
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
        let html = render_login_page(&test_config(), "", &test_config().self_url);
        // No placeholders remain.
        assert!(!html.contains("{{ "));
        assert!(!html.contains(" {{"));
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
        let data_dir = std::path::Path::new("no-such-data-dir");
        // test_get_theme_path_nonexistent
        assert_eq!(
            theme_path(&docroot, data_dir, "nonexistent-theme-xyz", ""),
            ""
        );
        // test_get_theme_path_with_default
        assert_eq!(
            theme_path(
                &docroot,
                data_dir,
                "nonexistent-theme-xyz",
                "themes/classic"
            ),
            "themes/classic"
        );
        // test_get_theme_path_empty_default
        assert_eq!(
            theme_path(&docroot, data_dir, "nonexistent-theme-xyz", ""),
            ""
        );
    }

    #[test]
    fn theme_css_link_matches_php() {
        let docroot = test_config().docroot;
        let data_dir = std::path::Path::new("no-such-data-dir");
        // Empty pref → no link.
        assert_eq!(theme_css_link(&docroot, data_dir, ""), "");
        // Missing theme file → no link (`theme_exists` false).
        assert_eq!(
            theme_css_link(&docroot, data_dir, "nonexistent-theme-xyz"),
            ""
        );
        // Real theme → `stylesheet_tag` link with mtime cache-buster.
        if docroot.join("themes/night.css").is_file() {
            let link = theme_css_link(&docroot, data_dir, "night.css");
            assert!(link.starts_with("<link href=\"themes/night.css?"));
            assert!(link.contains(" rel=\"stylesheet\" type=\"text/css\""));
            assert!(link.ends_with("id=\"theme_css\"/>"));
        }
    }

    #[test]
    fn data_dir_theme_resolves_and_links() {
        // User themes live in `<data_dir>/themes.local` (docs/19 volume
        // model); the URL keeps the docroot-relative shape.
        let docroot = test_config().docroot;
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("themes.local").join("mytheme.css");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, "body { color: red; }").unwrap();

        assert_eq!(
            theme_path(docroot.as_path(), tmp.path(), "mytheme.css", ""),
            format!("themes.local/mytheme.css?{}", mtime_secs(&local))
        );
        let link = theme_css_link(docroot.as_path(), tmp.path(), "mytheme.css");
        assert!(link.starts_with("<link href=\"themes.local/mytheme.css?"));
        // A docroot theme still wins over the data dir (the PHP iteration
        // order: `themes` before the local dir).
        let shadowed = tmp.path().join("themes.local").join("night.css");
        std::fs::write(&shadowed, "body { }").unwrap();
        if docroot.join("themes/night.css").is_file() {
            let link = theme_css_link(docroot.as_path(), tmp.path(), "night.css");
            assert!(link.starts_with("<link href=\"themes/night.css?"));
        }
    }

    #[test]
    fn shell_and_prefs_emit_user_stylesheet() {
        let css = ".content img { max-width: 100%; max-height: 80vh; }";
        let block =
            format!("<style type='text/css' id='user_css_style'>{}</style>", css);

        // Non-empty custom CSS → the inline `<style>` block appears in both the
        // shell and the prefs page (PHP `UserHelper::print_user_stylesheet`).
        let shell = render_shell("tok", 123, "", &block, "", "", "", "", "", "", false);
        assert!(shell.contains(&block), "shell missing the user stylesheet block");
        let prefs = render_prefs_page("tok", 123, 3, "", &block, "", "", "");
        assert!(prefs.contains(&block), "prefs missing the user stylesheet block");

        // Empty → no `user_css_style` block is emitted.
        assert!(!render_shell("tok", 123, "", "", "", "", "", "", "", "", false).contains("user_css_style"));
        assert!(!render_prefs_page("tok", 123, 3, "", "", "", "", "").contains("user_css_style"));
    }

    /// Plugin CSS/JS appear in the shell when provided, and are absent when
    /// empty (inactive plugins contribute nothing).
    #[test]
    fn shell_renders_plugin_assets_when_present() {
        let css = ".my-plugin { color: red; }";
        let js = "Plugins.MyPlugin = {};";
        let shell = render_shell("tok", 123, "", "", css, js, "", "", "", "", false);
        assert!(shell.contains(css), "shell missing plugin CSS");
        assert!(shell.contains(js), "shell missing plugin JS");
        // Empty assets → no plugin CSS/JS in the shell.
        let shell_empty = render_shell("tok", 123, "", "", "", "", "", "", "", "", false);
        assert!(!shell_empty.contains("Plugins."), "empty shell should have no plugin JS");
    }

    /// `HOOK_MAIN_TOOLBAR_BUTTON` output lands between the order-0 and order-5
    /// toolbar markers (PHP `index.php:195`), and is absent when the row is
    /// empty (no active plugin emits a button).
    #[test]
    fn shell_renders_main_toolbar_buttons_between_markers() {
        let marker_before = "<!-- order 0, default -->";
        let marker_after = "<!-- order 5: alert icons -->";
        let button = "<button id=\"sidebar-toggle-button\" class=\"toggle-sidebar-label\"></button>";
        let shell = render_shell("tok", 123, "", "", "", "", button, "", "", "", false);
        let before = shell.find(marker_before).expect("order-0 marker present");
        let at = shell.find(button).expect("toolbar button row present");
        let after = shell.find(marker_after).expect("order-5 marker present");
        assert!(before < at && at < after, "button row must sit between the toolbar markers");
        // Empty row → nothing between the markers.
        let shell_empty = render_shell("tok", 123, "", "", "", "", "", "", "", "", false);
        let b = shell_empty.find(marker_before).expect("order-0 marker present");
        let a = shell_empty.find(marker_after).expect("order-5 marker present");
        assert!(
            shell_empty[b + marker_before.len()..a].trim().is_empty(),
            "empty row leaves nothing but whitespace between the markers"
        );
    }

    /// The prefs page takes the active plugins' *prefs* assets (`get_prefs_css`/
    /// `get_prefs_js` — PHP `prefs.php`), not their shell assets
    /// (`get_css`/`get_js`). Regression: the port once fed `share`'s shell JS
    /// (with `shareArticle`) into the prefs page instead of its prefs JS
    /// (`clearKeys`).
    #[test]
    fn prefs_receives_prefs_assets_not_shell_assets() {
        let prefs_js = "<script type=\"text/javascript\">\n\t\ttry {\n\t\t\t\tdocument.getElementById('share-clear-keys').onclick = SharePlugin.clearKeys;\n\t\t} catch (e) {\n\t\t\t\t\tconsole.warn('failed to initialize plugin JS: share', e);\n\t\t}\n\t</script>";
        let prefs_css = "#share-plugin { background: #eee; }";
        let prefs = render_prefs_page("tok", 123, 3, "", "", prefs_js, prefs_css, "");
        assert!(prefs.contains("clearKeys"), "prefs page missing the share prefs JS");
        assert!(!prefs.contains("shareArticle"), "prefs page must not receive the shell JS");
        assert!(prefs.contains(prefs_css), "prefs page missing the prefs CSS");
        // Empty → no `<script>` block and an empty `<style>` slot.
        let empty = render_prefs_page("tok", 123, 3, "", "", "", "", "");
        assert!(!empty.contains("failed to initialize plugin JS"), "no plugin JS expected");
    }

    #[tokio::test]
    async fn shell_translates_strings_for_non_english_locale() {
        let i18n = std::sync::Arc::new(crate::i18n::I18nCache::new(std::path::PathBuf::from("../../tt-rss/locale")));
        // English (no scope): the original string is present.
        let en = render_shell("tok", 123, "", "", "", "", "", "", "", "", false);
        assert!(en.contains("Loading, please wait..."), "English shell should have the original string");
        // German (scoped): the string should be translated.
        let de = crate::i18n::I18N
            .scope(
                crate::i18n::I18nCtx { locale: Some("de_DE".to_string()), cache: i18n },
                async { render_shell("tok", 123, "", "", "", "", "", "", "", "", false) },
            )
            .await;
        assert!(!de.contains("Loading, please wait..."), "German shell should not have the English string");
        assert!(de.contains("Ladevorgang, bitte warten..."), "German shell should contain the translated string");
    }

    #[tokio::test]
    async fn login_page_translates_for_non_english_locale() {
        let i18n = std::sync::Arc::new(crate::i18n::I18nCache::new(std::path::PathBuf::from("../../tt-rss/locale")));
        // English (no scope): the original string is present.
        let en = render_login_page(&test_config(), "", &test_config().self_url);
        assert!(en.contains("Log in"), "English login should have the original string");
        // German (scoped): the string should be translated.
        let de = crate::i18n::I18N
            .scope(
                crate::i18n::I18nCtx { locale: Some("de_DE".to_string()), cache: i18n },
                async { render_login_page(&test_config(), "", &test_config().self_url) },
            )
            .await;
        assert!(!de.contains(">Log in<"), "German login should not have the English button text");
        assert!(de.contains(">Anmelden<"), "German login should contain the translated button text");
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
