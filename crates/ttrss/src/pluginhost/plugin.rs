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

use async_trait::async_trait;
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
    /// A full response (custom status + Content-Type) — PHP's public methods
    /// that `header(…)` / set the Content-Type (`share::get`: the article HTML
    /// on a hit, a 404 for a missing key). The backend route folds this into
    /// its raw status + body; the public route honors both.
    Response {
        status: u16,
        content_type: &'static str,
        body: String,
    },
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
#[async_trait]
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
    async fn authenticate(
        &self,
        _host: &mut HostCtx,
        _login: &str,
        _password: &str,
        _service: &str,
    ) -> Option<i32> {
        None
    }

    // -- dynamic method dispatch ----------------------------------------------

    /// Dispatch for PHP's dynamic `$plugin->$method(...)` calls: the
    /// `PluginHandler` backend (`op=PluginHandler&plugin=..&method=..`) and
    /// `update.php` CLI commands both invoke arbitrary plugin methods by
    /// name. `params` is the request/CLI argument map (PHP's `$params` /
    /// `$args`).
    async fn invoke_method(
        &self,
        _host: &mut HostCtx,
        method: &str,
        _params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        Err(PluginError::UnknownMethod(method.to_string()))
    }

    // -- hooks ------------------------------------------------------------------
    // PHP order (classes/Plugin.php); default bodies mirror the PHP dummies.

    /// `hook_article_button($line)` — buttons for an article row (right
    /// side); returned markup must be valid XML. `HOOK_ARTICLE_BUTTON`.
    async fn hook_article_button(&self, _host: &mut HostCtx, _line: &Value) -> String {
        String::new()
    }

    /// `hook_article_filter($article)` — alter article data as gathered from
    /// feed XML (embed images, full text, …). `HOOK_ARTICLE_FILTER`.
    async fn hook_article_filter(&self, _host: &mut HostCtx, _article: &mut Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_prefs_tab($tab)` — add UI elements to (top) preferences tabs.
    /// `HOOK_PREFS_TAB`.
    async fn hook_prefs_tab(&self, _host: &mut HostCtx, _tab: &str) -> () {}

    /// `hook_prefs_tab_section($section)` — add content to preferences
    /// sections (e.g. the OPML import/export pane). `HOOK_PREFS_TAB_SECTION`.
    async fn hook_prefs_tab_section(&self, _host: &mut HostCtx, _section: &str) -> () {}

    /// `hook_prefs_tabs()` — add new (top) preferences tabs.
    /// `HOOK_PREFS_TABS`.
    async fn hook_prefs_tabs(&self, _host: &mut HostCtx) -> () {}

    /// `hook_feed_parsed($parser, $feed_id)` — invoked while a feed's XML is
    /// processed by the parser. `HOOK_FEED_PARSED`.
    async fn hook_feed_parsed(&self, _host: &mut HostCtx, _parser: &FeedParser<'_>, _feed_id: i32) -> () {}

    /// `hook_update_task($cli_options)` — **GLOBAL** (system plugins only):
    /// invoked when a feed update task finishes. `HOOK_UPDATE_TASK`.
    async fn hook_update_task(&self, _host: &mut HostCtx, _cli_options: &Value) -> () {}

    /// `hook_auth_user($login, $password, $service)` — the `HOOK_AUTH_USER`
    /// compatibility wrapper; the base implementation delegates to
    /// [`Plugin::authenticate`]. `HOOK_AUTH_USER`.
    async fn hook_auth_user(
        &self,
        host: &mut HostCtx,
        login: &str,
        password: &str,
        service: &str,
    ) -> Option<i32> {
        self.authenticate(host, login, password, service).await
    }

    /// `hook_hotkey_map($hotkeys)` — modify the global hotkey map
    /// (sequence → action). `HOOK_HOTKEY_MAP`.
    async fn hook_hotkey_map(&self, _host: &mut HostCtx, _hotkeys: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_render_article($article)` — article rendered for the three-panel
    /// mode. `HOOK_RENDER_ARTICLE`.
    async fn hook_render_article(&self, _host: &mut HostCtx, _article: &mut Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_render_article_cdm($article)` — article rendered for combined
    /// (CDM) mode. `HOOK_RENDER_ARTICLE_CDM`.
    async fn hook_render_article_cdm(&self, _host: &mut HostCtx, _article: &mut Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_feed_fetched($feed_data, $fetch_url, $owner_uid, $feed)` — raw
    /// feed XML downloaded (not yet parsed). `HOOK_FEED_FETCHED`.
    async fn hook_feed_fetched(
        &self,
        _host: &mut HostCtx,
        _feed_data: &str,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed: i32,
    ) -> String {
        String::new()
    }

    /// `hook_sanitize($doc, $site_url, $allowed_elements, $disallowed_attributes, $article_id)`
    /// — article content while it is sanitized. `HOOK_SANITIZE`.
    ///
    /// `doc` is the article HTML (the port's sanitizer is string-based; PHP
    /// passes a `DOMDocument`).
    async fn hook_sanitize(
        &self,
        _host: &mut HostCtx,
        _doc: &mut String,
        _site_url: &str,
        _allowed_elements: &[String],
        _disallowed_attributes: &[String],
        _article_id: i32,
    ) -> () {}

    /// `hook_render_article_api($params)` — article rendered for API clients
    /// (`params` = `{article, headline}`). `HOOK_RENDER_ARTICLE_API`.
    async fn hook_render_article_api(&self, _host: &mut HostCtx, _params: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_toolbar_button()` — main toolbar (right, before Actions…).
    /// `HOOK_TOOLBAR_BUTTON`.
    async fn hook_toolbar_button(&self, _host: &mut HostCtx) -> String {
        String::new()
    }

    /// `hook_action_item()` — main Actions… dropdown items.
    /// `HOOK_ACTION_ITEM`.
    async fn hook_action_item(&self, _host: &mut HostCtx) -> String {
        String::new()
    }

    /// `hook_headline_toolbar_button($feed_id, $is_cat)` — toolbar area of the
    /// loaded feed's headlines. `HOOK_HEADLINE_TOOLBAR_BUTTON`.
    async fn hook_headline_toolbar_button(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _is_cat: bool,
    ) -> String {
        String::new()
    }

    /// `hook_hotkey_info($hotkeys)` — add hotkey action names/descriptions.
    /// `HOOK_HOTKEY_INFO`.
    async fn hook_hotkey_info(&self, _host: &mut HostCtx, _hotkeys: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_article_left_button($row)` — per-article buttons (left side);
    /// markup must be valid XML. `HOOK_ARTICLE_LEFT_BUTTON`.
    async fn hook_article_left_button(&self, _host: &mut HostCtx, _row: &Value) -> String {
        String::new()
    }

    /// `hook_prefs_edit_feed($feed_id)` — UI in the "Plugins" tab of the feed
    /// editor. `HOOK_PREFS_EDIT_FEED`.
    async fn hook_prefs_edit_feed(&self, _host: &mut HostCtx, _feed_id: i32) -> () {}

    /// `hook_prefs_save_feed($feed_id)` — feed editor data saved.
    /// `HOOK_PREFS_SAVE_FEED`. The `params` is the form data (equivalent of
    /// PHP's `$_POST`), so plugins can read the posted checkbox/select values.
    async fn hook_prefs_save_feed(&self, _host: &mut HostCtx, _feed_id: i32, _params: &Value) -> () {}

    /// `hook_fetch_feed(...)` — override the built-in feed fetch, substituting
    /// the received data. `HOOK_FETCH_FEED`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    async fn hook_fetch_feed(
        &self,
        _host: &mut HostCtx,
        _feed_data: &str,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed: i32,
        _last_article_timestamp: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        String::new()
    }

    /// `hook_query_headlines($row, $excerpt_length)` — headlines row fetched
    /// from the database. `HOOK_QUERY_HEADLINES`.
    async fn hook_query_headlines(&self, _host: &mut HostCtx, _row: &mut Value, _excerpt_length: i32) -> Value {
        Value::Array(vec![])
    }

    /// `hook_house_keeping()` — run periodically by the update daemon when
    /// idle (user plugins only for active users). `HOOK_HOUSE_KEEPING`.
    async fn hook_house_keeping(&self, _host: &mut HostCtx) -> () {}

    /// `hook_search($query)` — override the built-in article search; returns
    /// `(SQL query, highlight keywords)`. `HOOK_SEARCH`.
    async fn hook_search(&self, _host: &mut HostCtx, _query: &str) -> Value {
        Value::Array(vec![])
    }

    /// `hook_format_enclosures($enclosures_formatted, $enclosures, $article_id,
    /// $always_display_enclosures, $article_content, $hide_images)` —
    /// enclosures rendered to HTML; may return either the formatted string or
    /// the `(formatted, enclosures)` pair. `HOOK_FORMAT_ENCLOSURES`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    async fn hook_format_enclosures(
        &self,
        _host: &mut HostCtx,
        _enclosures_formatted: &str,
        _enclosures: &Value,
        _article_id: i32,
        _always_display_enclosures: bool,
        _article_content: &str,
        _hide_images: bool,
    ) -> Value {
        Value::String(String::new())
    }

    /// `hook_subscribe_feed($contents, $url, $auth_login, $auth_pass)` — feed
    /// subscription, after data has been fetched. `HOOK_SUBSCRIBE_FEED`.
    async fn hook_subscribe_feed(
        &self,
        _host: &mut HostCtx,
        _contents: &str,
        _url: &str,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        String::new()
    }

    /// `hook_headlines_before($feed, $is_cat, $qfh_ret)` — markup before the
    /// headlines. `HOOK_HEADLINES_BEFORE`.
    async fn hook_headlines_before(
        &self,
        _host: &mut HostCtx,
        _feed: i32,
        _is_cat: bool,
        _qfh_ret: &Value,
    ) -> String {
        String::new()
    }

    /// `hook_render_enclosure($entry, $article_id, $rv)` — render one
    /// enclosure entry. `HOOK_RENDER_ENCLOSURE`.
    async fn hook_render_enclosure(
        &self,
        _host: &mut HostCtx,
        _entry: &Value,
        _article_id: i32,
        _rv: &Value,
    ) -> String {
        String::new()
    }

    /// `hook_article_filter_action($article, $action)` — a custom filter
    /// action (registered via [`PluginHost::add_filter_action`]) is applied.
    /// `HOOK_ARTICLE_FILTER_ACTION`.
    async fn hook_article_filter_action(
        &self,
        _host: &mut HostCtx,
        _article: &mut Value,
        _action: &str,
    ) -> Value {
        Value::Array(vec![])
    }

    /// `hook_article_export_feed($line, $feed, $is_cat, $owner_uid)` —
    /// article exported to a syndication feed. `HOOK_ARTICLE_EXPORT_FEED`.
    async fn hook_article_export_feed(
        &self,
        _host: &mut HostCtx,
        _line: &mut Value,
        _feed: i32,
        _is_cat: bool,
        _owner_uid: i32,
    ) -> Value {
        Value::Array(vec![])
    }

    /// `hook_main_toolbar_button()` — main toolbar (left side).
    /// `HOOK_MAIN_TOOLBAR_BUTTON`.
    async fn hook_main_toolbar_button(&self, _host: &mut HostCtx) -> () {}

    /// `hook_enclosure_entry($entry, $id, $rv)` — every enclosure entry as the
    /// article is rendered. `HOOK_ENCLOSURE_ENTRY`.
    async fn hook_enclosure_entry(
        &self,
        _host: &mut HostCtx,
        _entry: &mut Value,
        _id: i32,
        _rv: &Value,
    ) -> Value {
        Value::Array(vec![])
    }

    /// `hook_format_article($html, $row)` — article rendered as HTML for
    /// sharing. `HOOK_FORMAT_ARTICLE`.
    async fn hook_format_article(&self, _host: &mut HostCtx, _html: &str, _row: &Value) -> String {
        String::new()
    }

    /// `hook_feed_basic_info($basic_info, $fetch_url, $owner_uid, $feed_id,
    /// $auth_login, $auth_pass)` — basic feed info (title, site_url) being
    /// collected; may override. `HOOK_FEED_BASIC_INFO`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    async fn hook_feed_basic_info(
        &self,
        _host: &mut HostCtx,
        basic_info: &mut Value,
        _fetch_url: &str,
        _owner_uid: i32,
        _feed_id: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> Value {
        basic_info.clone()
    }

    /// `hook_send_local_file($filename)` — file sent to the client; may
    /// override with an httpd-specific mechanism (nginx_xaccel).
    /// `HOOK_SEND_LOCAL_FILE`.
    async fn hook_send_local_file(&self, _host: &mut HostCtx, _filename: &str) -> bool {
        false
    }

    /// `hook_unsubscribe_feed($feed_id, $owner_uid)` — returning `true`
    /// prevents the default unsubscribe actions. `HOOK_UNSUBSCRIBE_FEED`.
    async fn hook_unsubscribe_feed(&self, _host: &mut HostCtx, _feed_id: i32, _owner_uid: i32) -> bool {
        false
    }

    /// `hook_send_mail($mailer, $params)` — mail being sent; fallback is
    /// PHP's `mail()` (in the port: the `lettre` Mailer, not yet ported).
    /// `HOOK_SEND_MAIL`.
    ///
    /// NOTE(M7): provisional signature — re-typed when the `Mailer` lands.
    async fn hook_send_mail(&self, _host: &mut HostCtx, _params: &Value) -> i32 {
        -1
    }

    /// `hook_filter_triggered($feed_id, $owner_uid, $article, $matched_filters,
    /// $matched_rules, $article_filter_actions)` — filtering triggered on an
    /// article. `HOOK_FILTER_TRIGGERED`.
    #[allow(clippy::too_many_arguments)] // mirrors the PHP signature
    async fn hook_filter_triggered(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _owner_uid: i32,
        _article: &Value,
        _matched_filters: &Value,
        _matched_rules: &Value,
        _article_filter_actions: &Value,
    ) -> () {}

    /// `hook_get_full_text($url)` — plugins may provide full article text.
    /// `HOOK_GET_FULL_TEXT`.
    async fn hook_get_full_text(&self, _host: &mut HostCtx, _url: &str) -> String {
        String::new()
    }

    /// `hook_article_image($enclosures, $content, $site_url, $article)` —
    /// article flavor image being determined; may override the selection.
    /// `HOOK_ARTICLE_IMAGE`.
    async fn hook_article_image(
        &self,
        _host: &mut HostCtx,
        _enclosures: &Value,
        _content: &str,
        _site_url: &str,
        _article: &Value,
    ) -> Value {
        Value::String(String::new())
    }

    /// `hook_feed_tree()` — arbitrary elements before the feed tree.
    /// `HOOK_FEED_TREE`.
    async fn hook_feed_tree(&self, _host: &mut HostCtx) -> String {
        String::new()
    }

    /// `hook_iframe_whitelisted($url)` — an iframe is allowed to be displayed.
    /// `HOOK_IFRAME_WHITELISTED`.
    async fn hook_iframe_whitelisted(&self, _host: &mut HostCtx, _url: &str) -> bool {
        false
    }

    /// `hook_enclosure_imported($enclosure, $feed)` — enclosure imported (e.g.
    /// via OPML). `HOOK_ENCLOSURE_IMPORTED`.
    async fn hook_enclosure_imported(&self, _host: &mut HostCtx, _enclosure: &mut Value, _feed: i32) -> Value {
        Value::Null
    }

    /// `hook_headlines_custom_sort_map()` — custom entries (name → caption) in
    /// the headline sort dropdown. `HOOK_HEADLINES_CUSTOM_SORT_MAP`.
    async fn hook_headlines_custom_sort_map(&self, _host: &mut HostCtx) -> Value {
        serde_json::json!({ "": "" })
    }

    /// `hook_headlines_custom_sort_override($order)` — override headline
    /// sorting; returns `(query, skip_first_id)`.
    /// `HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE`.
    async fn hook_headlines_custom_sort_override(&self, _host: &mut HostCtx, _order: &str) -> Value {
        serde_json::json!(["", false])
    }

    /// `hook_headline_toolbar_select_menu_item($feed_id, $is_cat)` — **deprecated
    /// in PHP** (removed; see `…_item2`), kept for contract parity.
    /// `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM`.
    async fn hook_headline_toolbar_select_menu_item(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _is_cat: bool,
    ) -> String {
        String::new()
    }

    /// `hook_headline_toolbar_select_menu_item2($feed_id, $is_cat)` — custom
    /// `<option>` elements in the headlines Select… dropdown.
    /// `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM2`.
    async fn hook_headline_toolbar_select_menu_item2(
        &self,
        _host: &mut HostCtx,
        _feed_id: i32,
        _is_cat: bool,
    ) -> String {
        String::new()
    }

    /// `hook_pre_subscribe($url, $auth_login, $auth_pass)` — user subscribing
    /// to a feed; may override the information used afterwards.
    /// `HOOK_PRE_SUBSCRIBE`.
    async fn hook_pre_subscribe(
        &self,
        _host: &mut HostCtx,
        _url: &mut String,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> bool {
        false
    }

    /// `hook_post_logout($login, $user_id)` — after logout; `[0]`, if set, is
    /// the redirect URL. `HOOK_POST_LOGOUT`.
    async fn hook_post_logout(&self, _host: &mut HostCtx, _login: &str, _user_id: i32) -> Value {
        serde_json::json!([""])
    }

    /// `hook_loginform_additional_buttons()` — buttons to the right of the
    /// default Login button. `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`.
    async fn hook_loginform_additional_buttons(&self, _host: &mut HostCtx) -> () {}

    /// `hook_validate_session()` — `false` marks the session invalid, `true`
    /// cascades to the next handler. `HOOK_VALIDATE_SESSION`.
    async fn hook_validate_session(&self, _host: &mut HostCtx) -> bool {
        false
    }

    /// `hook_articles_mark_toggled($article_ids)` — articles marked/unmarked
    /// (resulting state is not passed; read it from the DB).
    /// `HOOK_ARTICLES_MARK_TOGGLED`.
    async fn hook_articles_mark_toggled(&self, _host: &mut HostCtx, _article_ids: &[i32]) -> () {}

    /// `hook_articles_publish_toggled($article_ids)` — articles
    /// published/unpublished. `HOOK_ARTICLES_PUBLISH_TOGGLED`.
    async fn hook_articles_publish_toggled(&self, _host: &mut HostCtx, _article_ids: &[i32]) -> () {}
}
