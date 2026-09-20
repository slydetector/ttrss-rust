//! The `af_zz_vidmute` plugin (`tt-rss/plugins/af_zz_vidmute/init.php`, M7.8) —
//! mute audio in HTML5 videos in article content. Pure frontend: `init`
//! registers no hooks, and the single asset is `init.js` — a `dojo/ready`
//! block that client-side registers `HOOK_ARTICLE_RENDERED` /
//! `HOOK_ARTICLE_RENDERED_CDM` to set `muted = true` on every `<video>` in the
//! rendered row. PHP's `get_js` reads that file from the plugin directory; the
//! port `include_str!`s the same file so the frozen reference stays the single
//! source of truth. There is no `init.css` (PHP's `get_css` is the empty
//! default).
//!
//! The asset reaches the browser via the shell's per-plugin asset pass (M7.21
//! — the served page concatenates the **active** plugins' `get_js`/`get_css`);
//! until that lands the shell carries the golden dataset's static blocks, so
//! M7.8's own bar is that `get_js` returns the reference bytes verbatim.

use crate::pluginhost::host::PluginHost;
use crate::pluginhost::plugin::{About, Plugin};

/// PHP's `class Af_Zz_VidMute extends Plugin`.
#[derive(Clone, Copy)]
pub struct AfZzVidMute;

impl Plugin for AfZzVidMute {
    fn name(&self) -> &'static str {
        "af_zz_vidmute"
    }
    fn class_name(&self) -> &'static str {
        "Af_Zz_VidMute"
    }
    fn about(&self) -> About {
        About::new("Mute audio in HTML5 videos", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    /// PHP's `init($host)` is empty — no hooks.
    fn init(&self, _host: &mut PluginHost) {}
    /// PHP: `file_get_contents(__DIR__ . "/init.js")`.
    fn get_js(&self) -> String {
        INIT_JS.to_string()
    }
}

/// `tt-rss/plugins/af_zz_vidmute/init.js` (byte-identical; the reference
/// `get_js` is a `file_get_contents` of this file).
const INIT_JS: &str =
    include_str!("../../../../tt-rss/plugins/af_zz_vidmute/init.js");

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_file(name: &str) -> String {
        // Anchor on the crate manifest dir so the path is CWD-independent
        // (`cargo test` runs from `crates/ttrss`).
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tt-rss/plugins/af_zz_vidmute/",
        );
        std::fs::read_to_string(format!("{path}{name}")).unwrap()
    }

    #[test]
    fn metadata() {
        let p = AfZzVidMute;
        assert_eq!(p.name(), "af_zz_vidmute");
        assert_eq!(p.class_name(), "Af_Zz_VidMute");
        assert_eq!(p.about().description, "Mute audio in HTML5 videos");
        assert_eq!(p.about().author, "fox");
        assert_eq!(p.about().version, None);
        assert!(!p.about().system);
        assert_eq!(p.api_version(), 2);
    }

    #[test]
    fn js_is_the_reference_file() {
        // The reference `get_js` is `file_get_contents(init.js)` — byte-exact.
        assert_eq!(AfZzVidMute.get_js(), ref_file("init.js"));
    }

    #[test]
    fn css_is_empty() {
        // No `init.css` in the reference — `get_css` is the empty default.
        assert_eq!(AfZzVidMute.get_css(), "");
    }

    #[test]
    fn js_shape() {
        // The frontend surface the shell asset pass will emit (M7.21).
        let js = AfZzVidMute.get_js();
        assert!(js.contains("HOOK_ARTICLE_RENDERED_CDM"));
        assert!(js.contains("HOOK_ARTICLE_RENDERED"));
        assert!(js.contains("vid.muted = true"));
    }
}
