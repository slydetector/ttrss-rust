//! M7.16 — the **`toggle_sidebar`** plugin (a main toolbar button that
//! toggles the sidebar open/closed).
//!
//! PHP reference: `tt-rss/plugins/toggle_sidebar/init.php` (34 LOC).
//!
//! The plugin adds a single button to the main toolbar (the left-side
//! toolbar in the header). Clicking it calls `Plugins.Toggle_Sidebar.toggle()`,
//! which calls `Feeds.toggle()` (toggles the feed tree panel) and flips
//! the button icon between `chevron_left` and `chevron_right`.

use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::HOOK_MAIN_TOOLBAR_BUTTON;
use crate::i18n::{__};

/// The `toggle_sidebar` plugin.
#[derive(Clone, Copy)]
pub struct ToggleSidebar;

impl Plugin for ToggleSidebar {
    fn name(&self) -> &'static str {
        "toggle_sidebar"
    }
    fn class_name(&self) -> &'static str {
        "Toggle_Sidebar"
    }
    fn about(&self) -> About {
        About::new("Adds a main toolbar button to toggle sidebar", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_MAIN_TOOLBAR_BUTTON);
    }

    fn get_js(&self) -> String {
        r#"/* global Plugins, Feeds */

Plugins.Toggle_Sidebar = {
    toggle: function() {
        Feeds.toggle();

        const label = document.querySelector("i.toggle-sidebar-label");

        label.innerHTML = Element.visible("feeds-holder") ? 'chevron_left' : 'chevron_right';
    }
};"#
        .to_string()
    }

    fn hook_main_toolbar_button<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let toggle_title = __("Toggle sidebar");
            host.output.push_str(&format!(
                "<button dojoType=\"dijit.form.Button\" \
                 onclick=\"Plugins.Toggle_Sidebar.toggle(this)\">\
                 <i class=\"material-icons toggle-sidebar-label\"\
                 title=\"{toggle_title}\">chevron_left</i>\
                 </button>",
            ));
        })
    }
}
