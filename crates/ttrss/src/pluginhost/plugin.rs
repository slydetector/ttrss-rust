//! The plugin contract — port of `tt-rss/classes/Plugin.php` (M7.1).
//!
//! PHP's `Plugin` base class is the contract every plugin implements; the
//! `PluginHost` (see [`super::host`]) invokes the hook methods dynamically by
//! name. Rust has no dynamic method dispatch, so the contract is an explicit
//! trait: every hook is a method with a default no-op mirroring the PHP dummy
//! body (PHP's dummies `user_error(...)` — there is nothing to report when no
//! plugin implements a hook), and the host invokes the trait methods
//! statically in the same priority order.
//!
//! **Data surface (locked, M7 decision #8).** Hook parameters and return
//! values are PHP-shaped: scalars, `String`, and `serde_json::Value` for PHP
//! associative arrays / mixed. This keeps the contract ABI-friendly for a
//! later WASM loader and 1:1 with the PHP signatures.
//!
//! **Deliberate deviations from PHP:**
//! - PHP's gettext helpers (`__`, `_ngettext`, `T_sprintf`) are not on the
//!   trait: `__()` is identity in the port (locked decision #6), so plugin
//!   strings are plain literals.
//! - PHP wraps each plugin invocation in `try/catch` (a misbehaving plugin
//!   logs a warning and the chain continues). Rust plugins compile into the
//!   server; a panic is a host bug, so there is no per-plugin isolation.
//! - `hook_sanitize` takes the article HTML as `&mut String` (the port's
//!   sanitizer is string-based, not a `DOMDocument`); the call site (M7.6)
//!   maps PHP's `chain_hooks_callback($doc, ...)` onto it.
//! - `hook_send_mail` is provisional: PHP's `Mailer` object is not yet
//!   ported (the `lettre` milestone); the signature is re-typed when it lands.
//!
//! **Async surface (M7.3 substrate).** The methods that run inside a live
//! request — every `hook_*`, [`Plugin::invoke_method`],
//! [`Plugin::authenticate`] — are async (`BoxFuture`): ported plugins do DB
//! I/O (storage, queue tables), which is impossible from a synchronous trait
//! method on an async (`sqlx`) stack. The identity/registration methods
//! (`name`, `class_name`, `about`, `init`, `api_version`, `flags`), the pure
//! gates (`is_public_method`, `csrf_ignore`), and the page-asset accessors
//! (`get_js`, …) stay synchronous.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::core::feed::FeedParser;

use super::host::{HostCtx, PluginHost};

/// A dyn-compatible async-trait return type: a boxed `Send` future. Kept
/// object-safe on purpose (the registry stores `Box<dyn Plugin>`); the
/// `Send` bound is what lets the runners await across plugin invocations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The body a dynamic plugin method **prints** (PHP's `$plugin->$method()`
/// takes no return value — the method `print`s, and whatever it prints is
/// the response body).
#[derive(Debug, Clone, PartialEq)]
pub enum MethodOutput {
    /// The method printed a JSON document (PHP's `print json_encode(…)`).
    Json(Value),
    /// The method printed raw text/HTML (plain `print`/echo — e.g. the
    /// `note` plugin's dialog body, fed to a Dojo dialog via innerHTML).
    Raw(String),
}

/// `Plugin::API_VERSION_COMPAT` (classes/Plugin.php:3) — the version a plugin
/// that does not override `api_version()` advertises.
pub const API_VERSION_COMPAT: i32 = 1;

/// `Plugin::about()` (classes/Plugin.php).
///
/// PHP shape: `[version, description, author, is_system, more_info]` —
/// elements 0 and 4 are optional (`$about[3] ?? false`, `$about[4] ?? ''`).
/// Index 3 (`is_system`) gates the `KIND_SYSTEM`/`KIND_USER` loading.
#[derive(Debug, Clone, PartialEq)]
pub struct About {
    /// `about[0]` — e.g. `1.0`; `None` when absent (most first-party plugins).
    pub version: Option<f64>,
    /// `about[1]`.
    pub description: String,
    /// `about[2]`.
    pub author: String,
    /// `about[3] ?? false` — `PluginHost::is_system()`.
    pub system: bool,
    /// `about[4] ?? ""` — the "more info" URL in the plugin list.
    pub more_info: String,
}

impl About {
    pub fn new(description: &str, author: &str) -> Self {
        Self {
            version: None,
            description: description.to_string(),
            author: author.to_string(),
            system: false,
            more_info: String::new(),
        }
    }
}

/// `Plugin::flags()` (classes/Plugin.php) — `array<string, bool>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// `needs_curl` — the only key the reference documents.
    pub needs_curl: bool,
}

/// Errors a plugin method dispatch can report (PHP: `user_error` + the
/// `PluginHandler` JSON envelopes, M7.3).
#[derive(Debug, PartialEq, Eq)]
pub enum PluginError {
    /// The plugin does not implement the requested method.
    UnknownMethod(String),
    /// A plugin-reported failure (message shown to the user in PHP).
    Other(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMethod(m) => write!(f, "unknown plugin method: {m}"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for PluginError {}

/// The plugin contract — `tt-rss/classes/Plugin.php`, 1:1.
///
/// `Send + Sync`: the registry ([`PluginHost`]) is built once at startup and
/// shared across the server's worker threads; plugins are stateless across
/// requests (locked decision #4) and keep any mutable state in interior
/// mutability.
pub trait Plugin: Send + Sync {
    /// The lowercase plugin name — PHP's `plugins[]` map key (the class
    /// basename, lowercased in `load()`). This is the name that appears in
    /// `TTRSS_PLUGINS`, `_ENABLED_PLUGINS`, the plugin-listing JSON, and the
    /// `backend.php`/`public.php` URLs.
    fn name(&self) -> &'static str;

    /// The PHP class name exactly as declared (e.g. `"Nsfw"`). PHP stores
    /// `$sender::class` (original case) in `ttrss_plugin_storage.name` and in
    /// scheduled-task names; M7.2/M7.12 interop needs it.
    fn class_name(&self) -> &'static str;

    /// `about()` — the plugin's metadata (description/author/system flag).
    fn about(&self) -> About;

    /// `init($host)` — called once per plugin at host startup (PHP: once per
    /// host load; the PHP singleton is per-request, so effectively
    /// per-request there, once at startup in the port). Registration only:
    /// `add_hook`, `add_handler`, `add_command`, `add_filter_action`,
    /// `add_api_method`, `add_scheduled_task` — no I/O, no state that other
    /// requests can observe.
    fn init(&self, host: &mut PluginHost);

    /// `api_version()` — plugins below [`super::host::API_VERSION`] are
    /// skipped at load time (all first-party plugins override this to 2).
    fn api_version(&self) -> i32 {
        API_VERSION_COMPAT
    }

    fn flags(&self) -> Flags {
        Flags::default()
    }

    /// `is_public_method($method)` — `public.php` (the no-account endpoint)
    /// only invokes methods for which this returns `true`.
    fn is_public_method(&self, _method: &str) -> bool {
        false
    }

    /// `csrf_ignore($method)` — the `PluginHandler` backend skips the CSRF
    /// check for these methods.
    fn csrf_ignore(&self, _method: &str) -> bool {
        false
    }

    /// `method_exists($plugin, $method)` — the dispatch handlers
    /// ([`super::dispatch`]) probe method existence *before* their CSRF
    /// gates, so an unknown method yields `E_UNKNOWN_METHOD` even with a
    /// bad/missing token. Plugins enumerate exactly the methods their
    /// [`Plugin::invoke_method`] dispatches.
    fn has_public_method(&self, _method: &str) -> bool {
        false
    }
    // -- page-asset injection ------------------------------------------------

    fn get_js(&self) -> String {
        String::new()
    }
    fn get_login_js(&self) -> String {
        String::new()
    }
    fn get_css(&self) -> String {
        String::new()
    }
    fn get_prefs_js(&self) -> String {
        String::new()
    }
    fn get_prefs_css(&self) -> String {
        String::new()
    }

    // -- authentication (IAuthModule) ----------------------------------------

    /// `authenticate($login, $password, $service)` — the `HOOK_AUTH_USER`
    /// compatibility wrapper delegates to this (PHP's `Auth_Base`).
    fn authenticate<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _login: &'a str,
        _password: &'a str,
        _service: &'a str,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(async move {
        None
        })
    }

    // -- dynamic method dispatch ----------------------------------------------

    /// Dispatch for PHP's dynamic `$plugin->$method(...)` calls: the
    /// `PluginHandler` backend (`op=PluginHandler&plugin=..&method=..`) and
    /// `update.php` CLI commands both invoke arbitrary plugin methods by
    /// name. `params` is the request/CLI argument map (PHP's `$params` /
    /// `$args`).
    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        method: &'a str,
        _params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        Box::pin(async move {
        Err(PluginError::UnknownMethod(method.to_string()))
        })
    }

    // -- hooks ------------------------------------------------------------------
    // PHP order (classes/Plugin.php); default bodies mirror the PHP dummies.

    /// `hook_article_button($line)` — buttons for an article row (right
    /// side); returned markup must be valid XML. `HOOK_ARTICLE_BUTTON`.
    fn hook_article_button<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _line: &'a Value) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_article_filter($article)` — alter article data as gathered from
    /// feed XML (embed images, full text, …). `HOOK_ARTICLE_FILTER`.
    fn hook_article_filter<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _article: &'a mut Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_prefs_tab($tab)` — add UI elements to (top) preferences tabs.
    /// `HOOK_PREFS_TAB`.
    fn hook_prefs_tab<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _tab: &'a str) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_prefs_tab_section($section)` — add content to preferences
    /// sections (e.g. the OPML import/export pane). `HOOK_PREFS_TAB_SECTION`.
    fn hook_prefs_tab_section<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _section: &'a str) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_prefs_tabs()` — add new (top) preferences tabs.
    /// `HOOK_PREFS_TABS`.
    fn hook_prefs_tabs<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_feed_parsed($parser, $feed_id)` — invoked while a feed's XML is
    /// processed by the parser. `HOOK_FEED_PARSED`.
    fn hook_feed_parsed<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _parser: &'a FeedParser<'a>, _feed_id: i32) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_update_task($cli_options)` — **GLOBAL** (system plugins only):
    /// invoked when a feed update task finishes. `HOOK_UPDATE_TASK`.
    fn hook_update_task<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _cli_options: &'a Value) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_auth_user($login, $password, $service)` — the `HOOK_AUTH_USER`
    /// compatibility wrapper; the base implementation delegates to
    /// [`Plugin::authenticate`]. `HOOK_AUTH_USER`.
    fn hook_auth_user<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        login: &'a str,
        password: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(async move {
        self.authenticate(host, login, password, service).await
        })
    }

    /// `hook_hotkey_map($hotkeys)` — modify the global hotkey map
    /// (sequence → action). `HOOK_HOTKEY_MAP`.
    fn hook_hotkey_map<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _hotkeys: &'a Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_render_article($article)` — article rendered for the three-panel
    /// mode. `HOOK_RENDER_ARTICLE`.
    fn hook_render_article<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _article: &'a mut Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_render_article_cdm($article)` — article rendered for combined
    /// (CDM) mode. `HOOK_RENDER_ARTICLE_CDM`.
    fn hook_render_article_cdm<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _article: &'a mut Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_feed_fetched($feed_data, $fetch_url, $owner_uid, $feed)` — raw
    /// feed XML downloaded (not yet parsed). `HOOK_FEED_FETCHED`.
    fn hook_feed_fetched<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_data: &'a str,
        _fetch_url: &'a str,
        _owner_uid: i32,
        _feed: i32,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_sanitize($doc, $site_url, $allowed_elements, $disallowed_attributes, $article_id)`
    /// — article content while it is sanitized. `HOOK_SANITIZE`.
    ///
    /// `doc` is the article HTML (the port's sanitizer is string-based; PHP
    /// passes a `DOMDocument`).
    fn hook_sanitize<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _doc: &'a mut String,
        _site_url: &'a str,
        _allowed_elements: &'a [String],
        _disallowed_attributes: &'a [String],
        _article_id: i32,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {})
    }

    /// `hook_render_article_api($params)` — article rendered for API clients
    /// (`params` = `{article, headline}`). `HOOK_RENDER_ARTICLE_API`.
    fn hook_render_article_api<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _params: &'a Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_toolbar_button()` — main toolbar (right, before Actions…).
    /// `HOOK_TOOLBAR_BUTTON`.
    fn hook_toolbar_button<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_action_item()` — main Actions… dropdown items.
    /// `HOOK_ACTION_ITEM`.
    fn hook_action_item<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_headline_toolbar_button($feed_id, $is_cat)` — toolbar area of the
    /// loaded feed's headlines. `HOOK_HEADLINE_TOOLBAR_BUTTON`.
    fn hook_headline_toolbar_button<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_id: i32,
        _is_cat: bool,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_hotkey_info($hotkeys)` — add hotkey action names/descriptions.
    /// `HOOK_HOTKEY_INFO`.
    fn hook_hotkey_info<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _hotkeys: &'a Value) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_article_left_button($row)` — per-article buttons (left side);
    /// markup must be valid XML. `HOOK_ARTICLE_LEFT_BUTTON`.
    fn hook_article_left_button<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _row: &'a Value) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_prefs_edit_feed($feed_id)` — UI in the "Plugins" tab of the feed
    /// editor. `HOOK_PREFS_EDIT_FEED`.
    fn hook_prefs_edit_feed<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _feed_id: i32) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_prefs_save_feed($feed_id)` — feed editor data saved.
    /// `HOOK_PREFS_SAVE_FEED`.
    fn hook_prefs_save_feed<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _feed_id: i32) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_fetch_feed(...)` — override the built-in feed fetch, substituting
    /// the received data. `HOOK_FETCH_FEED`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    fn hook_fetch_feed<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_data: &'a str,
        _fetch_url: &'a str,
        _owner_uid: i32,
        _feed: i32,
        _last_article_timestamp: i32,
        _auth_login: &'a str,
        _auth_pass: &'a str,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_query_headlines($row, $excerpt_length)` — headlines row fetched
    /// from the database. `HOOK_QUERY_HEADLINES`.
    fn hook_query_headlines<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _row: &'a mut Value, _excerpt_length: i32) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_house_keeping()` — run periodically by the update daemon when
    /// idle (user plugins only for active users). `HOOK_HOUSE_KEEPING`.
    fn hook_house_keeping<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_search($query)` — override the built-in article search; returns
    /// `(SQL query, highlight keywords)`. `HOOK_SEARCH`.
    fn hook_search<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _query: &'a str) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_format_enclosures($enclosures_formatted, $enclosures, $article_id,
    /// $always_display_enclosures, $article_content, $hide_images)` —
    /// enclosures rendered to HTML; may return either the formatted string or
    /// the `(formatted, enclosures)` pair. `HOOK_FORMAT_ENCLOSURES`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    fn hook_format_enclosures<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _enclosures_formatted: &'a str,
        _enclosures: &'a Value,
        _article_id: i32,
        _always_display_enclosures: bool,
        _article_content: &'a str,
        _hide_images: bool,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::String(String::new())
        })
    }

    /// `hook_subscribe_feed($contents, $url, $auth_login, $auth_pass)` — feed
    /// subscription, after data has been fetched. `HOOK_SUBSCRIBE_FEED`.
    fn hook_subscribe_feed<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _contents: &'a str,
        _url: &'a str,
        _auth_login: &'a str,
        _auth_pass: &'a str,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_headlines_before($feed, $is_cat, $qfh_ret)` — markup before the
    /// headlines. `HOOK_HEADLINES_BEFORE`.
    fn hook_headlines_before<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed: i32,
        _is_cat: bool,
        _qfh_ret: &'a Value,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_render_enclosure($entry, $article_id, $rv)` — render one
    /// enclosure entry. `HOOK_RENDER_ENCLOSURE`.
    fn hook_render_enclosure<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _entry: &'a Value,
        _article_id: i32,
        _rv: &'a Value,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_article_filter_action($article, $action)` — a custom filter
    /// action (registered via [`PluginHost::add_filter_action`]) is applied.
    /// `HOOK_ARTICLE_FILTER_ACTION`.
    fn hook_article_filter_action<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _article: &'a mut Value,
        _action: &'a str,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_article_export_feed($line, $feed, $is_cat, $owner_uid)` —
    /// article exported to a syndication feed. `HOOK_ARTICLE_EXPORT_FEED`.
    fn hook_article_export_feed<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _line: &'a mut Value,
        _feed: i32,
        _is_cat: bool,
        _owner_uid: i32,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_main_toolbar_button()` — main toolbar (left side).
    /// `HOOK_MAIN_TOOLBAR_BUTTON`.
    fn hook_main_toolbar_button<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_enclosure_entry($entry, $id, $rv)` — every enclosure entry as the
    /// article is rendered. `HOOK_ENCLOSURE_ENTRY`.
    fn hook_enclosure_entry<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _entry: &'a mut Value,
        _id: i32,
        _rv: &'a Value,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Array(vec![])
        })
    }

    /// `hook_format_article($html, $row)` — article rendered as HTML for
    /// sharing. `HOOK_FORMAT_ARTICLE`.
    fn hook_format_article<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _html: &'a str, _row: &'a Value) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_feed_basic_info($basic_info, $fetch_url, $owner_uid, $feed_id,
    /// $auth_login, $auth_pass)` — basic feed info (title, site_url) being
    /// collected; may override. `HOOK_FEED_BASIC_INFO`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    fn hook_feed_basic_info<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        basic_info: &'a mut Value,
        _fetch_url: &'a str,
        _owner_uid: i32,
        _feed_id: i32,
        _auth_login: &'a str,
        _auth_pass: &'a str,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        basic_info.clone()
        })
    }

    /// `hook_send_local_file($filename)` — file sent to the client; may
    /// override with an httpd-specific mechanism (nginx_xaccel).
    /// `HOOK_SEND_LOCAL_FILE`.
    fn hook_send_local_file<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _filename: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
        false
        })
    }

    /// `hook_unsubscribe_feed($feed_id, $owner_uid)` — returning `true`
    /// prevents the default unsubscribe actions. `HOOK_UNSUBSCRIBE_FEED`.
    fn hook_unsubscribe_feed<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _feed_id: i32, _owner_uid: i32) -> BoxFuture<'a, bool> {
        Box::pin(async move {
        false
        })
    }

    /// `hook_send_mail($mailer, $params)` — mail being sent; fallback is
    /// PHP's `mail()` (in the port: the `lettre` Mailer, not yet ported).
    /// `HOOK_SEND_MAIL`.
    ///
    /// NOTE(M7): provisional signature — re-typed when the `Mailer` lands.
    fn hook_send_mail<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _params: &'a Value) -> BoxFuture<'a, i32> {
        Box::pin(async move {
        -1
        })
    }

    /// `hook_filter_triggered($feed_id, $owner_uid, $article, $matched_filters,
    /// $matched_rules, $article_filter_actions)` — filtering triggered on an
    /// article. `HOOK_FILTER_TRIGGERED`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    fn hook_filter_triggered<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_id: i32,
        _owner_uid: i32,
        _article: &'a Value,
        _matched_filters: &'a Value,
        _matched_rules: &'a Value,
        _article_filter_actions: &'a Value,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {})
    }

    /// `hook_get_full_text($url)` — plugins may provide full article text.
    /// `HOOK_GET_FULL_TEXT`.
    fn hook_get_full_text<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _url: &'a str) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_article_image($enclosures, $content, $site_url, $article)` —
    /// article flavor image being determined; may override the selection.
    /// `HOOK_ARTICLE_IMAGE`.
    fn hook_article_image<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _enclosures: &'a Value,
        _content: &'a str,
        _site_url: &'a str,
        _article: &'a Value,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::String(String::new())
        })
    }

    /// `hook_feed_tree()` — arbitrary elements before the feed tree.
    /// `HOOK_FEED_TREE`.
    fn hook_feed_tree<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_iframe_whitelisted($url)` — an iframe is allowed to be displayed.
    /// `HOOK_IFRAME_WHITELISTED`.
    fn hook_iframe_whitelisted<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _url: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
        false
        })
    }

    /// `hook_enclosure_imported($enclosure, $feed)` — enclosure imported (e.g.
    /// via OPML). `HOOK_ENCLOSURE_IMPORTED`.
    fn hook_enclosure_imported<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _enclosure: &'a mut Value, _feed: i32) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        Value::Null
        })
    }

    /// `hook_headlines_custom_sort_map()` — custom entries (name → caption) in
    /// the headline sort dropdown. `HOOK_HEADLINES_CUSTOM_SORT_MAP`.
    fn hook_headlines_custom_sort_map<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        serde_json::json!({ "": "" })
        })
    }

    /// `hook_headlines_custom_sort_override($order)` — override headline
    /// sorting; returns `(query, skip_first_id)`.
    /// `HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE`.
    fn hook_headlines_custom_sort_override<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _order: &'a str) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        serde_json::json!(["", false])
        })
    }

    /// `hook_headline_toolbar_select_menu_item($feed_id, $is_cat)` — **deprecated
    /// in PHP** (removed; see `…_item2`), kept for contract parity.
    /// `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM`.
    fn hook_headline_toolbar_select_menu_item<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_id: i32,
        _is_cat: bool,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_headline_toolbar_select_menu_item2($feed_id, $is_cat)` — custom
    /// `<option>` elements in the headlines Select… dropdown.
    /// `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2`.
    fn hook_headline_toolbar_select_menu_item2<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_id: i32,
        _is_cat: bool,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
        String::new()
        })
    }

    /// `hook_pre_subscribe($url, $auth_login, $auth_pass)` — user subscribing
    /// to a feed; may override the information used afterwards.
    /// `HOOK_PRE_SUBSCRIBE`.
    fn hook_pre_subscribe<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _url: &'a mut String,
        _auth_login: &'a str,
        _auth_pass: &'a str,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
        false
        })
    }

    /// `hook_post_logout($login, $user_id)` — after logout; `[0]`, if set, is
    /// the redirect URL. `HOOK_POST_LOGOUT`.
    fn hook_post_logout<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _login: &'a str, _user_id: i32) -> BoxFuture<'a, Value> {
        Box::pin(async move {
        serde_json::json!([""])
        })
    }

    /// `hook_loginform_additional_buttons()` — buttons to the right of the
    /// default Login button. `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`.
    fn hook_loginform_additional_buttons<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_validate_session()` — `false` marks the session invalid, `true`
    /// cascades to the next handler. `HOOK_VALIDATE_SESSION`.
    fn hook_validate_session<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>) -> BoxFuture<'a, bool> {
        Box::pin(async move {
        false
        })
    }

    /// `hook_articles_mark_toggled($article_ids)` — articles marked/unmarked
    /// (resulting state is not passed; read it from the DB).
    /// `HOOK_ARTICLES_MARK_TOGGLED`.
    fn hook_articles_mark_toggled<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _article_ids: &'a [i32]) -> BoxFuture<'a, ()> { Box::pin(async move {}) }

    /// `hook_articles_publish_toggled($article_ids)` — articles
    /// published/unpublished. `HOOK_ARTICLES_PUBLISH_TOGGLED`.
    fn hook_articles_publish_toggled<'a, 'h: 'a>(&'a self, _host: &'a mut HostCtx<'h>, _article_ids: &'a [i32]) -> BoxFuture<'a, ()> { Box::pin(async move {}) }
}
