//! The plugin system (M7) — port of `tt-rss/classes/Plugin.php` +
//! `tt-rss/classes/PluginHost.php`.
//!
//! - [`plugin`] — the `Plugin` trait: the contract every plugin implements
//!   (`name`/`class_name`/`about`/`init` + the ~54 hook methods with default
//!   no-ops, asset injection, `is_public_method`/`csrf_ignore`, and
//!   `invoke_method` for PHP's dynamic method dispatch).
//! - [`host`] — [`host::PluginHost`] (the shared registry, built once at
//!   startup) and [`host::HostCtx`] (the per-request/per-owner view: the
//!   active plugin set, owner/profile/access level, DB pool, special
//!   feeds), plus the four run modes and the registration API.
//!
//! M7.1 landed the machinery with an empty registry ([`all_plugins`] was
//! `[]`): no plugin ported, no hook call site wired, server behavior
//! unchanged (the no-op baseline). Plugins land in M7.4+ (each registers in
//! [`all_plugins`]; M7.4 is `note`); hook call sites are wired per increment
//! (M7.21 makes the remaining injection data-driven).
//!
//! See the plan's §3a Phase 7 for the increment breakdown and the locked
//! decisions (static loading, contract 1:1 with PHP, the WASM route
//! preserved).

pub mod dispatch;
pub mod host;
pub mod plugin;
pub mod storage;

pub use host::{
    method_url, public_method_url, feed_to_pfeed_id, pfeed_to_feed_id, scheduled_task_name,
    HostCtx, PluginHost,
};
pub use plugin::{About, Flags, Plugin, PluginError, API_VERSION_COMPAT};

/// `PluginHost::API_VERSION` — re-exported next to the registry for the
/// startup check.
pub const API_VERSION: i32 = host::API_VERSION;

// -- hook name constants (PHP `PluginHost::HOOK_*`, classes/PluginHost.php) --

pub const HOOK_ARTICLE_BUTTON: &str = "hook_article_button";
pub const HOOK_ARTICLE_FILTER: &str = "hook_article_filter";
pub const HOOK_PREFS_TAB: &str = "hook_prefs_tab";
pub const HOOK_PREFS_TAB_SECTION: &str = "hook_prefs_tab_section";
pub const HOOK_PREFS_TABS: &str = "hook_prefs_tabs";
pub const HOOK_FEED_PARSED: &str = "hook_feed_parsed";
pub const HOOK_UPDATE_TASK: &str = "hook_update_task";
pub const HOOK_AUTH_USER: &str = "hook_auth_user";
pub const HOOK_HOTKEY_MAP: &str = "hook_hotkey_map";
pub const HOOK_RENDER_ARTICLE: &str = "hook_render_article";
pub const HOOK_RENDER_ARTICLE_CDM: &str = "hook_render_article_cdm";
pub const HOOK_FEED_FETCHED: &str = "hook_feed_fetched";
pub const HOOK_SANITIZE: &str = "hook_sanitize";
pub const HOOK_RENDER_ARTICLE_API: &str = "hook_render_article_api";
pub const HOOK_TOOLBAR_BUTTON: &str = "hook_toolbar_button";
pub const HOOK_ACTION_ITEM: &str = "hook_action_item";
pub const HOOK_HEADLINE_TOOLBAR_BUTTON: &str = "hook_headline_toolbar_button";
pub const HOOK_HOTKEY_INFO: &str = "hook_hotkey_info";
pub const HOOK_ARTICLE_LEFT_BUTTON: &str = "hook_article_left_button";
pub const HOOK_PREFS_EDIT_FEED: &str = "hook_prefs_edit_feed";
pub const HOOK_PREFS_SAVE_FEED: &str = "hook_prefs_save_feed";
pub const HOOK_FETCH_FEED: &str = "hook_fetch_feed";
pub const HOOK_QUERY_HEADLINES: &str = "hook_query_headlines";
pub const HOOK_HOUSE_KEEPING: &str = "hook_house_keeping";
pub const HOOK_SEARCH: &str = "hook_search";
pub const HOOK_FORMAT_ENCLOSURES: &str = "hook_format_enclosures";
pub const HOOK_SUBSCRIBE_FEED: &str = "hook_subscribe_feed";
pub const HOOK_HEADLINES_BEFORE: &str = "hook_headlines_before";
pub const HOOK_RENDER_ENCLOSURE: &str = "hook_render_enclosure";
pub const HOOK_ARTICLE_FILTER_ACTION: &str = "hook_article_filter_action";
pub const HOOK_ARTICLE_EXPORT_FEED: &str = "hook_article_export_feed";
pub const HOOK_MAIN_TOOLBAR_BUTTON: &str = "hook_main_toolbar_button";
pub const HOOK_ENCLOSURE_ENTRY: &str = "hook_enclosure_entry";
pub const HOOK_FORMAT_ARTICLE: &str = "hook_format_article";
pub const HOOK_FEED_BASIC_INFO: &str = "hook_feed_basic_info";
pub const HOOK_SEND_LOCAL_FILE: &str = "hook_send_local_file";
pub const HOOK_UNSUBSCRIBE_FEED: &str = "hook_unsubscribe_feed";
pub const HOOK_SEND_MAIL: &str = "hook_send_mail";
pub const HOOK_FILTER_TRIGGERED: &str = "hook_filter_triggered";
pub const HOOK_GET_FULL_TEXT: &str = "hook_get_full_text";
pub const HOOK_ARTICLE_IMAGE: &str = "hook_article_image";
pub const HOOK_FEED_TREE: &str = "hook_feed_tree";
pub const HOOK_IFRAME_WHITELISTED: &str = "hook_iframe_whitelisted";
pub const HOOK_ENCLOSURE_IMPORTED: &str = "hook_enclosure_imported";
pub const HOOK_HEADLINES_CUSTOM_SORT_MAP: &str = "hook_headlines_custom_sort_map";
pub const HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE: &str = "hook_headlines_custom_sort_override";
pub const HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM: &str = "hook_headline_toolbar_select_menu_item";
pub const HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2: &str = "hook_headline_toolbar_select_menu_item2";
pub const HOOK_PRE_SUBSCRIBE: &str = "hook_pre_subscribe";
pub const HOOK_POST_LOGOUT: &str = "hook_post_logout";
pub const HOOK_LOGINFORM_ADDITIONAL_BUTTONS: &str = "hook_loginform_additional_buttons";
pub const HOOK_VALIDATE_SESSION: &str = "hook_validate_session";
pub const HOOK_ARTICLES_MARK_TOGGLED: &str = "hook_articles_mark_toggled";
pub const HOOK_ARTICLES_PUBLISH_TOGGLED: &str = "hook_articles_publish_toggled";

/// The static registration list (locked decision #1): every first-party
/// plugin the server ships, in the order PHP's `load_all` would see them
/// (alphabetical — PHP `asort`s the plugin directory listing).
///
/// M7.1: intentionally empty — the no-op baseline. Grows one entry per
/// plugin as M7.4+ land.
pub fn all_plugins() -> Vec<fn() -> Box<dyn Plugin>> {
    vec![
        || Box::new(crate::plugins::af_comics::AfComics),
        || Box::new(crate::plugins::af_psql_trgm::AfPsqlTrgm),
        || Box::new(crate::plugins::af_youtube_embed::AfYoutubeEmbed),
        || Box::new(crate::plugins::af_zz_vidmute::AfZzVidMute),
        || Box::new(crate::plugins::auth_internal::AuthInternal),
        || Box::new(crate::plugins::auth_remote::AuthRemote),
        || Box::new(crate::plugins::auto_assign_labels::AutoAssignLabels),
        || Box::new(crate::plugins::bookmarklets::Bookmarklets),
        || Box::new(crate::plugins::cache_starred_images::CacheStarredImages),
        || Box::new(crate::plugins::hotkeys_force_top::HotkeysForceTop),
        || Box::new(crate::plugins::hotkeys_noscroll::HotkeysNoscroll),
        || Box::new(crate::plugins::hotkeys_swap_jk::HotkeysSwapJk),
        || Box::new(crate::plugins::tweak_article_header::TweakArticleHeader),
        || Box::new(crate::plugins::no_iframes::NoIframes),
        || Box::new(crate::plugins::note::Note),
        || Box::new(crate::plugins::nsfw::NsFw),
        || Box::new(crate::plugins::share::Share),
        || Box::new(crate::plugins::shorten_expanded::ShortenExpanded),
        || Box::new(crate::plugins::toggle_sidebar::ToggleSidebar),
    ]
}


