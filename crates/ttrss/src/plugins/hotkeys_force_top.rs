//! M7.19 — the **`hotkeys_force_top`** plugin (force open article to the top).
//!
//! PHP reference: `tt-rss/plugins/hotkeys_force_top/init.php` (21 LOC) +
//! `init.js` (5 LOC).
//!
//! The plugin's `get_js` sets `Headlines.default_force_to_top = true`, which
//! makes the article panel scroll to the top when an article is opened
//! (instead of preserving the previous scroll position).

use crate::pluginhost::host::PluginHost;
use crate::pluginhost::plugin::{About, Plugin};
use async_trait::async_trait;

/// PHP's `class Hotkeys_Force_Top extends Plugin`.
#[derive(Clone, Copy)]
pub struct HotkeysForceTop;

#[async_trait]
impl Plugin for HotkeysForceTop {
    fn name(&self) -> &'static str {
        "hotkeys_force_top"
    }
    fn class_name(&self) -> &'static str {
        "Hotkeys_Force_Top"
    }
    fn about(&self) -> About {
        About::new("Force open article to the top", "itsamenathan")
    }
    fn api_version(&self) -> i32 {
        2
    }

    /// `init` is empty in the PHP (no hooks registered).
    fn init(&self, _host: &mut PluginHost) {}

    /// `get_js` — the `Headlines.default_force_to_top = true` line.
    fn get_js(&self) -> String {
        include_str!("../../../../tt-rss/plugins/hotkeys_force_top/init.js").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata() {
        let p = HotkeysForceTop;
        assert_eq!(p.name(), "hotkeys_force_top");
        assert_eq!(p.class_name(), "Hotkeys_Force_Top");
        assert_eq!(p.api_version(), 2);
    }

    #[test]
    fn js_sets_force_to_top() {
        let p = HotkeysForceTop;
        let js = p.get_js();
        assert!(js.contains("Headlines.default_force_to_top = true"));
    }
}
