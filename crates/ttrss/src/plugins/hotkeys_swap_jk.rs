//! M7.17 — the **`hotkeys_swap_jk`** plugin (swaps j/k hotkeys for vi users).
//!
//! PHP reference: `tt-rss/plugins/hotkeys_swap_jk/init.php` (28 LOC).
//!
//! The plugin registers `HOOK_HOTKEY_MAP` and swaps the j/k keys to their
//! uppercase equivalents: `j` → `next_feed`, `J` → `next_unread_feed`,
//! `k` → `prev_feed`, `K` → `prev_unread_feed` (the reverse of the default,
//! which has `j` = next article, `k` = prev article).

use crate::pluginhost::HOOK_HOTKEY_MAP;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, Plugin};
use async_trait::async_trait;
use serde_json::{Value, json};

/// PHP's `class Hotkeys_Swap_JK extends Plugin`.
#[derive(Clone, Copy)]
pub struct HotkeysSwapJk;

#[async_trait]
impl Plugin for HotkeysSwapJk {
    fn name(&self) -> &'static str {
        "hotkeys_swap_jk"
    }
    fn class_name(&self) -> &'static str {
        "Hotkeys_Swap_JK"
    }
    fn about(&self) -> About {
        About::new("Swap j and k hotkeys (for vi brethren)", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_HOTKEY_MAP);
    }

    /// `hook_hotkey_map($hotkeys)` — swap j/k to J/K (vi-style navigation).
    async fn hook_hotkey_map(&self, _host: &mut HostCtx, hotkeys: &Value) -> Value {
        let mut m = hotkeys.clone();
        let obj = m.as_object_mut().unwrap();
        obj.insert("j".into(), json!("next_feed"));
        obj.insert("J".into(), json!("next_unread_feed"));
        obj.insert("k".into(), json!("prev_feed"));
        obj.insert("K".into(), json!("prev_unread_feed"));
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn metadata() {
        let p = HotkeysSwapJk;
        assert_eq!(p.name(), "hotkeys_swap_jk");
        assert_eq!(p.class_name(), "Hotkeys_Swap_JK");
        assert_eq!(p.api_version(), 2);
    }

    #[tokio::test]
    async fn swaps_jk() {
        let host = Arc::new(PluginHost::startup(&[|| {
            Box::new(HotkeysSwapJk) as Box<dyn Plugin>
        }]));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let cfg = crate::core::config::Config::for_test();
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["hotkeys_swap_jk"],
        );
        let host = ctx.host_arc();

        let plugin = host.get_plugin("hotkeys_swap_jk").unwrap();

        let result = plugin.hook_hotkey_map(&mut ctx, &json!({})).await;
        assert_eq!(result["j"], "next_feed");
        assert_eq!(result["J"], "next_unread_feed");
        assert_eq!(result["k"], "prev_feed");
        assert_eq!(result["K"], "prev_unread_feed");
    }
}
