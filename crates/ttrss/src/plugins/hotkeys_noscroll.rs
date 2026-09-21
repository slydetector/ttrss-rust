//! M7.18 — the **`hotkeys_noscroll`** plugin (n/p hotkeys without scrolling).
//!
//! PHP reference: `tt-rss/plugins/hotkeys_noscroll/init.php` (27 LOC).
//!
//! The plugin registers `HOOK_HOTKEY_MAP` and maps the arrow keys and n/p
//! to the `*_noscroll` article-navigation actions (which move between
//! articles without scrolling the page).

use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::HOOK_HOTKEY_MAP;
use serde_json::{json, Value};

/// PHP's `class Hotkeys_Noscroll extends Plugin`.
#[derive(Clone, Copy)]
pub struct HotkeysNoscroll;

impl Plugin for HotkeysNoscroll {
    fn name(&self) -> &'static str {
        "hotkeys_noscroll"
    }
    fn class_name(&self) -> &'static str {
        "Hotkeys_Noscroll"
    }
    fn about(&self) -> About {
        About::new(
            "n/p (and up/down) hotkeys move between articles without scrolling",
            "fox",
        )
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_HOTKEY_MAP);
    }

    /// `hook_hotkey_map($hotkeys)` — map arrow keys + n/p to noscroll actions.
    fn hook_hotkey_map<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        hotkeys: &'a Value,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
            let mut m = hotkeys.clone();
            let obj = m.as_object_mut().unwrap();
            obj.insert("(40)|Down".into(), json!("next_article_noscroll"));
            obj.insert("(38)|Up".into(), json!("prev_article_noscroll"));
            obj.insert("n".into(), json!("next_article_noscroll"));
            obj.insert("p".into(), json!("prev_article_noscroll"));
            m
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata() {
        let p = HotkeysNoscroll;
        assert_eq!(p.name(), "hotkeys_noscroll");
        assert_eq!(p.class_name(), "Hotkeys_Noscroll");
        assert_eq!(p.api_version(), 2);
    }

    #[tokio::test]
    async fn maps_noscroll_keys() {
        let host = PluginHost::startup(&[|| Box::new(HotkeysNoscroll) as Box<dyn Plugin>]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let cfg = crate::core::config::Config::for_test();
        let mut ctx =
            HostCtx::for_user(&host, &pool, &cfg, Some(1), None, 0, &[], &["hotkeys_noscroll"]);
        let plugin = ctx.get_plugin("hotkeys_noscroll").unwrap();

        let result = plugin
            .hook_hotkey_map(&mut ctx, &json!({}))
            .await;
        assert_eq!(result["(40)|Down"], "next_article_noscroll");
        assert_eq!(result["(38)|Up"], "prev_article_noscroll");
        assert_eq!(result["n"], "next_article_noscroll");
        assert_eq!(result["p"], "prev_article_noscroll");
    }
}
