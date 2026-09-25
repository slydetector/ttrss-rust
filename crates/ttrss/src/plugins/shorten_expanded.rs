//! The `shorten_expanded` plugin (`tt-rss/plugins/shorten_expanded/init.php`,
//! M7.7) — collapse overly long articles in CDM/expanded view. Pure frontend:
//! `init` registers no hooks, and the page assets are `init.js` (a
//! `Plugins.Shorten_Expanded` object that truncates tall article bodies in
//! expanded/CDM and offers an "expand" prompt) and `init.css` (the
//! `.content-shrink-wrap` / `.expand-prompt` styles). PHP's `get_js`/`get_css`
//! read those files from the plugin directory; the port `include_str!`s the
//! same files so the frozen reference stays the single source of truth (the
//! assets are tab-indented exactly as on disk).
//!
//! The assets reach the browser via the shell's per-plugin asset pass (M7.21 —
//! the served page concatenates the **active** plugins' `get_js`/`get_css`);
//! until that lands the shell carries the golden dataset's static blocks, so
//! M7.7's own bar is that these methods return the reference bytes verbatim.

use crate::pluginhost::host::PluginHost;
use async_trait::async_trait;
use crate::pluginhost::plugin::{About, Plugin};

/// PHP's `class Shorten_Expanded extends Plugin`.
#[derive(Clone, Copy)]
pub struct ShortenExpanded;

#[async_trait]
impl Plugin for ShortenExpanded {
    fn name(&self) -> &'static str {
        "shorten_expanded"
    }
    fn class_name(&self) -> &'static str {
        "Shorten_Expanded"
    }
    fn about(&self) -> About {
        About::new("Shorten overly long articles in CDM/expanded", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    /// PHP's `init($host)` is empty — no hooks.
    fn init(&self, _host: &mut PluginHost) {}
    /// PHP: `file_get_contents(__DIR__ . "/init.css")`.
    fn get_css(&self) -> String {
        INIT_CSS.to_string()
    }
    /// PHP: `file_get_contents(__DIR__ . "/init.js")`.
    fn get_js(&self) -> String {
        INIT_JS.to_string()
    }
}

/// `tt-rss/plugins/shorten_expanded/init.css` (byte-identical; the reference
/// `get_css` is a `file_get_contents` of this file).
const INIT_CSS: &str =
    include_str!("../../../../tt-rss/plugins/shorten_expanded/init.css");
/// `tt-rss/plugins/shorten_expanded/init.js` (byte-identical; the reference
/// `get_js` is a `file_get_contents` of this file).
const INIT_JS: &str =
    include_str!("../../../../tt-rss/plugins/shorten_expanded/init.js");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata() {
        let p = ShortenExpanded;
        assert_eq!(p.name(), "shorten_expanded");
        assert_eq!(p.class_name(), "Shorten_Expanded");
        assert_eq!(
            p.about().description,
            "Shorten overly long articles in CDM/expanded"
        );
        assert_eq!(p.about().author, "fox");
        assert_eq!(p.about().version, None);
        assert!(!p.about().system);
        assert_eq!(p.api_version(), 2);
    }

    fn ref_file(name: &str) -> String {
        // Anchor on the crate manifest dir so the path is CWD-independent
        // (`cargo test` runs from `crates/ttrss`).
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tt-rss/plugins/shorten_expanded/",
        );
        std::fs::read_to_string(format!("{path}{name}")).unwrap()
    }

    #[test]
    fn js_is_the_reference_file() {
        // The reference `get_js` is `file_get_contents(init.js)` — byte-exact.
        assert_eq!(ShortenExpanded.get_js(), ref_file("init.js"));
    }

    #[test]
    fn css_is_the_reference_file() {
        assert_eq!(ShortenExpanded.get_css(), ref_file("init.css"));
    }

    #[test]
    fn js_shape() {
        // The frontend surface the shell asset pass will emit (M7.21).
        let js = ShortenExpanded.get_js();
        assert!(js.contains("Plugins.Shorten_Expanded = {"));
        assert!(js.contains("threshold: 1.5"));
        let css = ShortenExpanded.get_css();
        assert!(css.contains(".content-shrink-wrap"));
        assert!(css.contains(".expand-prompt"));
    }
}
