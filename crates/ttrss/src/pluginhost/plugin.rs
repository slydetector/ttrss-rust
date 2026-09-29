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
///
/// **Hook documentation rules** (keep every `Value`-parameter / `Value`-return
/// hook self-describing, so plugin authors never have to chase call sites):
/// 1. Line 1: the PHP signature + one-phrase purpose + the `HOOK_*` constant.
/// 2. Each `Value` param: one line — `param` — object/array + where the port
///    builds it; then **one bullet per field**:
///    `- \`field\` type — description` (flat; no grouped lines, no prose).
///    Variable-key maps (dictionaries): mark the param `variable keys` and
///    describe the key and value shapes as the bullets instead.
/// 3. Return: one line — what the core reads back (the contract) + fallback
///    semantics (e.g. "a missing key keeps the previous value").
/// 4. PHP-only keys (present in the PHP's value, never set by the port):
///    name them, state they are never set, and why (the port's analog, if
///    any) — never list them as if a plugin could receive them.
/// 5. Optional final line: a dispatch caveat (not dispatched / provisional)
///    with a pointer to where it's documented.
/// Field descriptions must be derived from the port's dispatch site and the
/// frozen PHP (constant values stated as such), never guessed. Shared shapes
/// are cross-referenced, not re-listed (e.g. the `FeedEnclosure` map in
/// `hook_enclosure_imported`).
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
    /// `hook_article_button($line)` — per-article buttons (right side);
    /// markup must be valid XML. `HOOK_ARTICLE_BUTTON`.
    ///
    /// PHP `Feeds.php:283` (`Feeds::view`) runs the chain per row: each
    /// result that parses as XML is tagged with `data-plugin-name` and
    /// appended to the row's `buttons` slot (invalid XML is logged and
    /// skipped). The port runs the same chain per row in
    /// `headlines.rs::article_buttons` (the headlines view) with the same
    /// XML round-trip; the concatenation lands in `buttons`.
    ///
    /// Parameters: `line` object — the headline row (PDO-string shape; the
    /// same field list as `hook_query_headlines`'s `row`).
    ///
    /// Return: the HTML appended to the row's `buttons` ("" = none).
    async fn hook_article_button(&self, _host: &mut HostCtx, _line: &Value) -> String {
        String::new()
    }

    /// `hook_article_filter($article)` — alter article data as gathered from
    /// feed XML (embed images, full text, …). `HOOK_ARTICLE_FILTER`.
    /// `hook_article_filter($article)` — rewrite an article before it is
    /// saved (update pipeline, after the hash check). `HOOK_ARTICLE_FILTER`.
    ///
    /// `article` — object (built in `updates.rs::upsert_article`):
    /// - `owner_uid` int — the feed owner's UID (read-only)
    /// - `guid` str — the article GUID: the item's guid, else its link, else
    ///   a title-derived one; prefixed `{owner_uid},`, max 245 chars
    /// - `guid_hashed` str — the stored guid column:
    ///   `{"ver":2,"uid":<owner>,"hash":"SHA1:<sha1 of guid>"}`
    /// - `title` str — the item title, tags stripped (the formatted
    ///   timestamp if the item had no title)
    /// - `content` str — the item content (raw HTML; the description if the
    ///   content was empty)
    /// - `link` str — the item link, relative URLs resolved against the
    ///   feed's site URL
    /// - `tags` array of str — the item's categories
    /// - `labels` array of str — the article's existing labels (empty for a
    ///   new article); the chain may add to these
    /// - `author` str — the item author, tags stripped, max 245 chars
    /// - `force_catchup` bool — always `false` on entry
    /// - `score_modifier` int — always `0` on entry
    /// - `language` str — the item language, trimmed, first 2 chars
    /// - `timestamp` int — the article date, epoch seconds (0 when missing)
    /// - `num_comments` int — the item's claimed comment count
    /// - `enclosures` array of `FeedEnclosure` objects (see `hook_enclosure_imported`)
    /// - `feed` object
    ///   - `id` int — the feed id
    ///   - `fetch_url` str — the feed's URL
    ///   - `site_url` str — the feed's site URL (the feed URL's host when
    ///     unset)
    ///   - `cache_images` bool — the feed's `cache_images` pref
    ///
    /// Return: the (possibly rewritten) object. The core re-reads `title`,
    /// `content`, `author`, `link`, `tags`, `labels` — a missing key keeps the
    /// pre-hook value.
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
    /// `hook_prefs_tabs()` — extra content in the preferences page's tab bar.
    /// `HOOK_PREFS_TABS`.
    ///
    /// PHP `prefs.php:181` runs `run_hooks(HOOK_PREFS_TABS)` while
    /// rendering the prefs page; the echo hooks' output lands in the tab
    /// bar markup. The port runs the same chain in `http.rs` (the prefs
    /// page route) and appends the concatenation to the tab bar.
    ///
    /// Parameters: none.
    ///
    /// Return: nothing (`()`); the hook echoes its HTML through the host
    /// (`host.output`).
    async fn hook_prefs_tabs(&self, _host: &mut HostCtx) -> () {}

    /// `hook_feed_parsed($parser, $feed_id)` — invoked while a feed's XML is
    /// processed by the parser. `HOOK_FEED_PARSED`.
    async fn hook_feed_parsed(&self, _host: &mut HostCtx, _parser: &FeedParser<'_>, _feed_id: i32) -> () {}

    /// `hook_update_task($cli_options)` — invoked when a feed update task
    /// finishes (daemon tick, after housekeeping). `HOOK_UPDATE_TASK`.
    ///
    /// `cli_options` — object (built in `updates.rs::run_update_task_hook`):
    /// - `daemon-loop` bool — always `true` (the only key the port sets)
    ///
    /// The PHP-only keys `quiet`/`log`/`log-level` (update.php CLI flags) are
    /// never set — the port's daemon has no such flags (its analogs are the
    /// `TTRSS_DAEMON` env switch and `RUST_LOG`); no first-party plugin reads
    /// them.
    ///
    /// Return: `()`.
    /// `hook_update_task($cli_options)` — update daemon task ran.
    /// `HOOK_UPDATE_TASK`.
    ///
    /// PHP `update.php:203/250` runs `run_hooks(HOOK_UPDATE_TASK, $options)`
    /// with the daemon process's CLI options map (after a batch, and after
    /// `housekeeping_common` in daemon mode). The port runs the same hook in
    /// `updates.rs::run_update_task_hook` on the global daemon host after
    /// housekeeping; the port's daemon is env-driven, so `cli_options` is
    /// `{"daemon-loop": true}` (PHP's `--task`/`--quiet`/… flags have no
    /// ported equivalent).
    ///
    /// Parameters: `cli_options` object — the daemon's CLI options (in the
    /// port, `{"daemon-loop": true}`).
    ///
    /// Return: nothing (`()`); the hook echoes its output through the host.
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
    /// `hook_hotkey_map($hotkeys)` — modify the global hotkey map before it
    /// is served. `HOOK_HOTKEY_MAP`.
    ///
    /// `hotkeys` — object, the global map (built in `hotkeys.rs::hotkeys_map`
    /// from the base table; variable keys):
    /// - key — the hotkey sequence str (e.g. `g a`, `ctrl+s`)
    /// - value — the action name str (e.g. `goto_all`, `toggle_cdm_expanded`)
    ///
    /// Return: the (possibly modified) map — the core serves it verbatim; a
    /// plugin may add sequences or redefine existing keys.
    /// `hook_hotkey_map($hotkeys)` — add/redefine keyboard shortcuts.
    /// `HOOK_HOTKEY_MAP`.
    ///
    /// PHP `RPC.php:629` (`get_hotkeys_map`) runs
    /// `chain_hooks_callback(HOOK_HOTKEY_MAP, ..., $hotkeys)` over the base
    /// hotkey map (sequence → action, e.g. `"g r" => "goto_read"`) *before*
    /// the prefixes are computed, so plugins can add or redefine keys. The
    /// port runs the same chain in `hotkeys.rs::hotkeys_map` (the API
    /// `getHotkeysMap` route), over the built-in map, before the prefix
    /// pass.
    ///
    /// Parameters: `hotkeys` object — the base hotkey map (key = key
    /// sequence str, value = action name str).
    ///
    /// Return: the (possibly altered) hotkey map — it *replaces* the base
    /// map (PHP's callback rebinds `$hotkeys = $result`).
    async fn hook_hotkey_map(&self, _host: &mut HostCtx, _hotkeys: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_render_article($article)` — article rendered for the three-panel
    /// mode. `HOOK_RENDER_ARTICLE`.
    /// `hook_render_article($article)` — article rendered for the public
    /// shared-article page (before the page is served). `HOOK_RENDER_ARTICLE`.
    ///
    /// `article` — object (built in `plugins/share.rs`, the shared-article
    /// route; a subset of the PHP's full entry row — share/init.php:126):
    /// - `id` int — the entry id
    /// - `title` str — the article title
    /// - `link` str — the article link
    /// - `lang` str — the entry language
    /// - `site_url` str — the feed's site URL
    /// - `content` str — the sanitized article HTML
    /// - `tags` array of str — the article's tags
    ///
    /// Return: the (possibly rewritten) object. The core reads back `content`
    /// (the page body; a missing key renders it empty) and passes the map to
    /// the og-image scan.
    /// `hook_render_article($article)` — article rendered for sharing.
    /// `HOOK_RENDER_ARTICLE`.
    ///
    /// PHP `share/init.php:149` (the share plugin's public article page)
    /// runs `chain_hooks_callback(HOOK_RENDER_ARTICLE, cb, $line)` over the
    /// sanitized article row before the page is rendered: `$line = $result`
    /// — the final result is the row the page uses. The port runs the same
    /// chain in `plugins/share.rs` (the share article route), after the
    /// content is sanitized.
    ///
    /// Parameters: `article` object (mutable) — the share row: `id` int,
    /// `title` str, `link` str, `lang` str|null, `site_url` str|null,
    /// `content` str (sanitized HTML), `tags` array of str.
    ///
    /// Return: the (possibly rewritten) row — it *replaces* the row for the
    /// rest of the share page.
    async fn hook_render_article(&self, _host: &mut HostCtx, _article: &mut Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_render_article_cdm($article)` — article rendered for combined
    /// (CDM) mode. `HOOK_RENDER_ARTICLE_CDM`.
    /// `hook_render_article_cdm($article)` — article rendered for the CDM
    /// (three-panel) view; runs per headline row before it is served.
    /// `HOOK_RENDER_ARTICLE_CDM`.
    ///
    /// `article` — object, the CDM headline row (built in
    /// `headlines.rs::view`; PHP `Feeds.php:351` passes the same full row):
    /// - `id` int — the entry id
    /// - `title` str — the article title
    /// - `updated` str — the article's updated time, localized
    /// - `always_display_enclosures` bool — the feed's pref (null: archived
    ///   row, no feed)
    /// - `site_url` str — the feed's site URL (null: archived row)
    /// - `note` str — the user's private note for the article (null: none)
    /// - `num_comments` int — the item's claimed comment count
    /// - `comments` str — the item's comments URL
    /// - `int_id` int — the user-entry row id
    /// - `lang` str — the entry language (null: none)
    /// - `hide_images` bool — the feed's pref (null: archived row)
    /// - `unread` bool — the user has not read the article
    /// - `feed_id` int — the feed id (-1: archived row)
    /// - `marked` bool — the user marked the article read
    /// - `published` bool — the article is pinned to the top
    /// - `link` str — the article URL
    /// - `last_read` str — when the user last read it (null: never)
    /// - `feed_title` str — the feed/vfeed title ("" when unset, "Archived
    ///   articles" when archived)
    /// - `content` str — the sanitized article HTML
    /// - `author` str — the item's author
    /// - `score` str — the user's score, a numeric string
    /// - `num_labels` int — labels assigned to the article
    /// - `content_preview` str — "— " + first 250 chars of plain text (""
    ///   when previews are off)
    /// - `labels` array — each element `[label id str, caption str,
    ///   fg_color str, bg_color str]`
    /// - `buttons_left` str — always `""` in the port (the PHP's
    ///   `HOOK_ARTICLE_LEFT_BUTTON` chain appends here; that hook is not
    ///   dispatched)
    /// - `buttons` str — the `HOOK_ARTICLE_BUTTON` chain output (HTML)
    /// - `enclosures` object — `formatted` str, `entries` (stored enclosure
    ///   rows + `filename`), `can_inline` bool, `inline_text_only` bool
    /// - `updated_long` str — the updated time, localized with weekday
    /// - `imported` str — "Imported at <date>"
    /// - `tags` array of str — the article's tags
    /// - `has_icon` bool — the feed has a favicon on disk
    /// - `feed_bg_color` str — the feed's headline background "rgba(r,g,b,0.3)"
    ///
    /// Return: the (possibly rewritten) object. The core serves the row
    /// verbatim to the UI. (The PHP rewrites cached-image URLs in `content`
    /// after the chain; the port's view route does not yet run
    /// `DiskCache::rewrite_urls`.)
    /// `hook_render_article_cdm($article)` — article rendered for the CDM
    /// (three-panel) view; runs per headline row before it is served.
    /// `HOOK_RENDER_ARTICLE_CDM`.
    ///
    /// PHP `Feeds.php:351` (`Feeds::view`) runs the chain per row, after
    /// `feed_bg_color` is computed and before `DiskCache::rewrite_urls`:
    /// `$line = $result` — the final result is the row, and PHP then strips
    /// `date_entered`/`guid`/`uuid`/… from it before serving. The port runs
    /// the same chain per row in `headlines.rs::view`, at the same position
    /// (the row served to the client is the hook's final result).
    ///
    /// Parameters: `article` object (mutable) — the headline row
    /// (PDO-string shape; the same field list as `hook_query_headlines`'s
    /// `row`, plus `has_icon` bool and `feed_bg_color` str by this point).
    ///
    /// Return: the (possibly rewritten) row — it *replaces* the row for the
    /// rest of the view loop.
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
    /// (before the API response is sent). `HOOK_RENDER_ARTICLE_API`.
    ///
    /// `params` — object (built in `api.rs::api_headline_row`, the
    /// getHeadlines route):
    /// - `article` object — the API article row (always set by the port):
    ///   - `id` int — the entry id
    ///   - `guid` str
    ///   - `unread` bool
    ///   - `marked` bool
    ///   - `published` bool
    ///   - `updated` int — epoch seconds (0 when missing)
    ///   - `is_updated` bool — never read and not unread
    ///   - `title` str
    ///   - `link` str
    ///   - `feed_id` int
    ///   - `tags` array of str
    ///   - `attachments` array — stored enclosure rows (only with
    ///     `includeAttachments`)
    ///   - `excerpt` str — plain-text preview (only with `showExcerpt`)
    ///   - `content` str — article HTML (only with `showContent`; sanitized
    ///     when the client passes `sanitize`)
    ///   - `flavor_image` str — the representative image URL
    ///   - `flavor_stream` str — the representative stream URL ("" if none)
    ///   - `flavor_kind` int — the representative media kind (only when
    ///     non-zero)
    ///   - `labels` array — each element `[label id str, caption str,
    ///     fg_color str, bg_color str]`
    ///   - `feed_title` str
    ///   - `comments_count` int
    ///   - `comments_link` str
    ///   - `always_display_attachments` bool — the feed's pref
    ///   - `author` str
    ///   - `score` str — a numeric string
    ///   - `note` str (null: none) — the user's private note
    ///   - `lang` str
    ///   - `site_url` str
    /// - `headline` object — the same row shape (PHP-only: PHP's
    ///   getHeadlines passes `headline`; the port passes `article` on both
    ///   routes)
    ///
    /// Dispatched on both API routes (getHeadlines — `api.rs::api_headline_row`;
    /// getArticle — `api.rs::get_article`); the PHP's `headline` key is
    /// never set by the port (its getHeadlines uses `article` too).
    ///
    /// Return: the (possibly rewritten) **article row** — not the `params`
    /// wrapper. The core replaces the row with the result. (The PHP then
    /// rewrites cached-image URLs in `content`; the port's API routes do
    /// not yet run `DiskCache::rewrite_urls`.)
    async fn hook_render_article_api(&self, _host: &mut HostCtx, _params: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_toolbar_button()` — main toolbar (right, before Actions…).
    /// `HOOK_TOOLBAR_BUTTON`.
    /// `hook_toolbar_button()` — main toolbar (right, before Actions…).
    /// `HOOK_TOOLBAR_BUTTON`.
    ///
    /// PHP `index.php:263` (the shell page) runs
    /// `run_hooks_callback(HOOK_TOOLBAR_BUTTON, echo)` — the hooks' HTML is
    /// echoed inline into the toolbar area. The port runs the same chain
    /// (`http.rs::shell_hook_strings`) and renders the concatenation into
    /// the shell template's toolbar slot (empty when no active plugin emits
    /// anything).
    ///
    /// Parameters: none.
    ///
    /// Return: the HTML to insert into the toolbar.
    async fn hook_toolbar_button(&self, _host: &mut HostCtx) -> String {
        String::new()
    }

    /// `hook_action_item()` — main Actions… dropdown items.
    /// `HOOK_ACTION_ITEM`.
    /// `hook_action_item()` — main Actions… dropdown items.
    /// `HOOK_ACTION_ITEM`.
    ///
    /// PHP `index.php:321` (the shell page) runs
    /// `run_hooks_callback(HOOK_ACTION_ITEM, echo)` — the hooks' HTML is
    /// echoed inline into the Actions… dropdown. The port runs the same
    /// chain (`http.rs::shell_hook_strings`) and renders the concatenation
    /// into the shell template's action-item slot (empty when no active
    /// plugin emits anything).
    ///
    /// Parameters: none.
    ///
    /// Return: the HTML to insert into the Actions… dropdown.
    async fn hook_action_item(&self, _host: &mut HostCtx) -> String {
        String::new()
    }

    /// `hook_headline_toolbar_button($feed_id, $is_cat)` — toolbar area of the
    /// loaded feed's headlines. `HOOK_HEADLINE_TOOLBAR_BUTTON`.
    /// `hook_headline_toolbar_button($feed_id, $is_cat)` — toolbar area of the
    /// loaded feed's headlines. `HOOK_HEADLINE_TOOLBAR_BUTTON`.
    ///
    /// PHP `Feeds.php:177` (`Feeds::view`) runs
    /// `chain_hooks_callback(..., $feed, $cat_view)` and appends the results
    /// to the view's `toolbar.plugin_buttons` slot. The port runs the same
    /// chain in `headlines.rs::view` (headlines view + API getHeadlines)
    /// and appends the concatenation to `toolbar.plugin_buttons`.
    ///
    /// Parameters:
    /// - `feed_id` int — the feed id of the view (0 for a category view)
    /// - `is_cat` bool — `true` when the view is a category (PHP's `$cat_view`)
    ///
    /// Return: the HTML to append to the toolbar's `plugin_buttons` slot.
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
    /// `hook_hotkey_info($hotkeys)` — add hotkey action names/descriptions.
    /// `HOOK_HOTKEY_INFO`.
    ///
    /// PHP `RPC.php:549` (`hotkey_help`) runs
    /// `chain_hooks_callback(HOOK_HOTKEY_INFO, ..., $hotkeys)` over the
    /// built-in hotkey info map (key → description) so plugins can add
    /// their own entries; the final result is the map the RPC returns. The
    /// port runs the same chain in `hotkeys.rs` (the API `hotkey_help`
    /// route), over the built-in map, before returning it.
    ///
    /// Parameters: `hotkeys` object — the current hotkey info map
    /// (key → description string; the built-in entries like
    /// `toggle_unread`, `next_article`, `help_dialog`, …).
    ///
    /// Return: the (possibly extended) hotkey info map — it *replaces* the
    /// built-in one (PHP's callback assigns `$hotkeys = $result`).
    async fn hook_hotkey_info(&self, _host: &mut HostCtx, _hotkeys: &Value) -> Value {
        Value::Array(vec![])
    }

    /// `hook_article_left_button($row)` — per-article buttons (left side);
    /// markup must be valid XML. `HOOK_ARTICLE_LEFT_BUTTON`.
    /// `hook_article_left_button($row)` — per-article buttons (left side);
    /// markup must be valid XML. `HOOK_ARTICLE_LEFT_BUTTON`.
    ///
    /// PHP `Feeds.php:258` (`Feeds::view`) runs the chain per row: each
    /// result that parses as XML is appended to the row's `buttons_left`
    /// slot (invalid XML is logged and skipped). The port runs the same
    /// chain per row in `headlines.rs::view` and appends the concatenation
    /// to `buttons_left` (it does not re-validate the XML — see the
    /// `view_pipeline_hooks` tests).
    ///
    /// Parameters: `row` object — the headline row (PDO-string shape; see
    /// `row_to_plugin_value` in `headlines.rs`): `id` str, `date_entered`
    /// str|null, `guid`, `title`, `updated` str|null, `label_cache`,
    /// `tag_cache`, `always_display_enclosures` "t"|"f"|null, `site_url`
    /// str|null, `note` str|null, `num_comments` str, `comments`, `int_id`
    /// str, `uuid`, `lang` str|null, `hide_images` "t"|"f"|null, `unread`
    /// "t"|"f", `feed_id` str|null (null: archived row), `marked` "t"|"f",
    /// `published` "t"|"f", `link`, `last_read` str|null (null: never),
    /// `last_marked` str|null, `last_published` str|null, `feed_title`
    /// str|null (category views), `favicon_avg_color` (category views),
    /// `content` (article HTML), `author`, `score` str, `num_labels` str,
    /// `num_enclosures` str, `content_preview`, plus `tags` array when set.
    ///
    /// Return: the HTML appended to the row's `buttons_left` ("" = none).
    async fn hook_article_left_button(&self, _host: &mut HostCtx, _row: &Value) -> String {
        String::new()
    }

    /// `hook_prefs_edit_feed($feed_id)` — UI in the "Plugins" tab of the feed
    /// editor. `HOOK_PREFS_EDIT_FEED`.
    async fn hook_prefs_edit_feed(&self, _host: &mut HostCtx, _feed_id: i32) -> () {}

    /// `hook_prefs_save_feed($feed_id)` — feed editor data saved.
    /// `HOOK_PREFS_SAVE_FEED`. The `params` is the form data (equivalent of
    /// PHP's `$_POST`), so plugins can read the posted checkbox/select values.
    /// `hook_prefs_save_feed($feed_id, $params)` — feed editor data saved.
    /// `HOOK_PREFS_SAVE_FEED`.
    ///
    /// PHP `Pref_Feeds.php:754` runs `run_hooks(HOOK_PREFS_SAVE_FEED,
    /// $feed_id)` after a feed is saved (echo hook — plugins read
    /// `$_POST` themselves). The port runs the same chain in
    /// `feeds.rs::save_feed` after the DB write, passing the posted form
    /// data as `params` (the port collects `$_POST` at the route, so the
    /// hook does not).
    ///
    /// Parameters:
    /// - `feed_id` int — the saved feed
    /// - `params` object — the posted feed-editor form data (the
    ///   `$_POST`-equivalent: `title`, `site_url`, the checkbox/select
    ///   values, …)
    ///
    /// Return: nothing (`()`); the hook echoes its output through the host.
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
    /// `hook_query_headlines($row, $excerpt_length)` — headlines row fetched
    /// from the database. `HOOK_QUERY_HEADLINES`.
    ///
    /// PHP `Feeds.php:215` (`Feeds::view`) runs the chain per row, right
    /// after `content_preview` is set: `$line = $result` — the result
    /// *replaces* the row, and the rest of the loop continues on the
    /// replaced row. The port runs the same chain per row in
    /// `headlines.rs::view` and in the syndication export
    /// (`syndication.rs`), with `excerpt_length` fixed at 250 (PHP's
    /// `$max_excerpt_length`).
    ///
    /// Parameters:
    /// - `row` object (mutable) — the headline row (PDO-string shape; see
    ///   `row_to_plugin_value` in `headlines.rs`): `id` str,
    ///   `date_entered` str|null, `guid`, `title`, `updated` str|null,
    ///   `label_cache`, `tag_cache`, `always_display_enclosures` "t"|"f"|null,
    ///   `site_url` str|null, `note` str|null, `num_comments` str,
    ///   `comments`, `int_id` str, `uuid`, `lang` str|null,
    ///   `hide_images` "t"|"f"|null, `unread` "t"|"f", `feed_id` str|null
    ///   (null: archived row), `marked` "t"|"f", `published` "t"|"f`,
    ///   `link`, `last_read` str|null (null: never), `last_marked` str|null,
    ///   `last_published` str|null, `feed_title` str|null (category views),
    ///   `favicon_avg_color` (category views), `content` (article HTML),
    ///   `author`, `score` str, `num_labels` str, `num_enclosures` str,
    ///   `content_preview`, plus `tags` array when set.
    /// - `excerpt_length` int — the `content_preview` truncation length
    ///   (250 at both call sites)
    ///
    /// Return: the (possibly rewritten) row — it *replaces* the row for
    /// the rest of the loop. The default (empty array) is treated as "no
    /// change" by the port's chain (PHP would install an empty row).
    async fn hook_query_headlines(&self, _host: &mut HostCtx, _row: &mut Value, _excerpt_length: i32) -> Value {
        Value::Array(vec![])
    }

    /// `hook_house_keeping()` — run periodically by the update daemon when
    /// idle (user plugins only for active users). `HOOK_HOUSE_KEEPING`.
    async fn hook_house_keeping(&self, _host: &mut HostCtx) -> () {}

    /// `hook_search($query)` — override the built-in article search.
    /// `HOOK_SEARCH`.
    ///
    /// Return: array `[0]` = SQL search query str, `[1]` = highlight-keywords
    /// array of str (the PHP contract, Plugin.php `hook_search`). An empty
    /// array declines (fall through to the built-in search).
    ///
    /// Not dispatched by the port: no stock plugin implements it, and the
    /// port calls `_search_to_sql` directly (behavior-identical; see
    /// `docs/php-port-gap-scan.md` G1).
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
    /// `hook_headlines_before($feed_id, $is_cat, $qfh_ret)` — markup before the
    /// headlines list. `HOOK_HEADLINES_BEFORE`.
    ///
    /// PHP `Feeds.php:195` (`Feeds::view`) runs
    /// `chain_hooks_callback(..., $feed, $cat_view, $qfh_ret)` *only on the
    /// first page* (`$offset == 0`) and appends the result into
    /// `$reply['content']` (which is `[]` at that point; the no-articles
    /// message later *replaces* it). The port runs the same chain on the
    /// first page in `headlines.rs::view` and prepends the result before
    /// the article rows (the no-articles path makes it the whole
    /// `content`).
    ///
    /// Parameters:
    /// - `feed_id` int — the feed id of the view (0 for a category view)
    /// - `is_cat` bool — `true` when the view is a category (PHP's `$cat_view`)
    /// - `qfh_ret` array — the fetched rows: `[row_count int, feed_title,
    ///   feed_site_url, last_error, last_updated, search_words, first_id,
    ///   is_vfeed, query_error_override]` (PHP's opaque `PDOStatement` is
    ///   the row count in the port)
    ///
    /// Return: the HTML to insert before the headlines list ("" = nothing).
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
    /// `hook_article_export_feed($row, $feed_id, $is_cat, $owner_uid)` —
    /// article exported to a syndication feed. `HOOK_ARTICLE_EXPORT_FEED`.
    ///
    /// PHP `Handler_Public.php:110/209` (the public syndication export) runs
    /// `chain_hooks_callback(..., $line, $feed, $is_cat, $owner_uid)` per
    /// exported row: `$line = $result` — the result *replaces* the row, and
    /// the export continues with the replaced row. The port runs the same
    /// chain per row in `syndication.rs`.
    ///
    /// Parameters:
    /// - `row` object (mutable) — the export row (same shape as
    ///   `hook_query_headlines`'s `row`)
    /// - `feed_id` int — the feed being exported
    /// - `is_cat` bool — `true` when a category is exported
    /// - `owner_uid` int — the exporting user
    ///
    /// Return: the (possibly rewritten) row — it *replaces* the row for
    /// this export. The default (empty array) is treated as "no change"
    /// by the port's chain.
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
    /// `hook_format_article($html, $row)` — article rendered as HTML for
    /// sharing. `HOOK_FORMAT_ARTICLE`.
    ///
    /// PHP `share/init.php:233` (the share plugin's article page) runs
    /// `chain_hooks_callback(HOOK_FORMAT_ARTICLE, cb, $rv, $line)` over the
    /// finished page HTML before printing it; `$rv = $result` — the final
    /// result is the page content. The port runs the same chain in
    /// `plugins/share.rs` (the share article route), over the finished
    /// page HTML; the final result replaces the page.
    ///
    /// Parameters:
    /// - `html` str — the finished share page HTML
    /// - `row` object — the shared article row (same shape as
    ///   `hook_render_article`'s `row`)
    ///
    /// Return: the (possibly rewritten) page HTML — the final result
    /// replaces the page.
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
    /// `hook_unsubscribe_feed($feed_id, $owner_uid)` — feed removal.
    /// `HOOK_UNSUBSCRIBE_FEED`.
    ///
    /// PHP `Pref_Feeds.php:1149` (`remove_feed`) runs
    /// `run_hooks_until(HOOK_UNSUBSCRIBE_FEED, true, $id, $owner_uid)`
    /// *before* the default removal — a hook returning `true` (and no later
    /// hook returning `false`) suppresses the built-in unsubscribe (the
    /// hook took over). The port runs the same chain in `feeds.rs::remove_feed`.
    ///
    /// Parameters:
    /// - `feed_id` int — the feed being removed
    /// - `owner_uid` int — the owning user
    ///
    /// Return: `true` — suppress the default removal; `false` (default) —
    /// proceed with it.
    async fn hook_unsubscribe_feed(&self, _host: &mut HostCtx, _feed_id: i32, _owner_uid: i32) -> bool {
        false
    }

    /// `hook_send_mail($mailer, $params)` — mail being sent; fallback is
    /// PHP's `mail()` (in the port: the `lettre` Mailer, not yet ported).
    /// `HOOK_SEND_MAIL`.
    ///
    /// NOTE(M7): provisional signature — re-typed when the `Mailer` lands.
    /// `hook_send_mail($params)` — mail being sent. `HOOK_SEND_MAIL`.
    ///
    /// PHP `Mailer.php:34` iterates the registered hooks (not a chain):
    /// `1`/`true` = handled (stop), `-1` = fatal (stop, no further action),
    /// anything else = try the next handler, falling through to PHP's
    /// `mail()`. The hook receives the `Mailer` instance (for `set_error`)
    /// plus the mail params.
    ///
    /// Not dispatched by the port: the `Mailer`/`lettre` milestone isn't in
    /// yet. NOTE(M7): provisional signature — re-typed (with the `Mailer`
    /// handle) when the `Mailer` lands.
    ///
    /// Parameters: `params` object — the mail (to/from/subject/body; pinned
    /// down when the `Mailer` lands).
    ///
    /// Return: `1` = handled, `-1` = fatal, `0`/other = no opinion (next
    /// handler). The default is `-1`.
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
    /// `hook_get_full_text($url)` — fetch an article's full text (RSS
    /// `enclosure`-less content) via a plugin. `HOOK_GET_FULL_TEXT`.
    ///
    /// PHP `Article.php:44` (`_get_full_text`) runs
    /// `run_hooks_callback(HOOK_GET_FULL_TEXT, cb, $url)` — the first
    /// *truthy* result replaces the fetched content and stops the chain;
    /// with no truthy result the built-in fetch runs. The port runs the
    /// same chain in `article.rs::create_published_article` (publishing an
    /// article to its own feed), with the owner-scoped host; the first
    /// non-empty result is used as the full text and the fetch is skipped.
    ///
    /// Parameters: `url` str — the article URL to fetch.
    ///
    /// Return: the full-text HTML ("" = no opinion; the built-in fetch
    /// runs).
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
    /// `hook_feed_tree()` — extra content in the feeds tree area.
    /// `HOOK_FEED_TREE`.
    ///
    /// PHP `index.php:175` (the shell page) runs
    /// `run_hooks_callback(HOOK_FEED_TREE, echo)` — the hooks' HTML is
    /// echoed inline into the feeds tree area. The port runs the same chain
    /// (`http.rs::shell_hook_strings`) and renders the concatenation into
    /// the shell template's feed-tree slot (empty when no active plugin
    /// emits anything).
    ///
    /// Parameters: none.
    ///
    /// Return: the HTML to insert into the feeds tree area.
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
    ///
    /// `enclosure` — object, the PHP's `FeedEnclosure`
    /// (tt-rss/classes/FeedEnclosure.php):
    /// - `link` str
    /// - `type` str
    /// - `length` str
    /// - `title` str
    /// - `height` str
    /// - `width` str
    ///
    /// Return: the (possibly rewritten) object. Read back field-by-field —
    /// a missing key keeps the enclosure's previous value.
    async fn hook_enclosure_imported(&self, _host: &mut HostCtx, _enclosure: &mut Value, _feed: i32) -> Value {
        Value::Null
    }

    /// `hook_headlines_custom_sort_map()` — custom entries (name → caption) in
    /// the headline sort dropdown. `HOOK_HEADLINES_CUSTOM_SORT_MAP`.
    /// `hook_headlines_custom_sort_map()` — extra headline sort options.
    /// `HOOK_HEADLINES_CUSTOM_SORT_MAP`.
    ///
    /// PHP `API.php:924` (`_get_custom_sort_types`) runs
    /// `run_hooks_callback(HOOK_HEADLINES_CUSTOM_SORT_MAP, ...)` with no
    /// arguments and *merges* every plugin's returned map
    /// (sort value → title) into the `custom_sort_types` config value the
    /// sort dropdown consumes. The port builds the same merged map in
    /// `api.rs::get_config` — `[]` when no plugin registers a map, an
    /// object when at least one does (PHP's empty-array/object encoding
    /// quirk preserved).
    ///
    /// Parameters: none.
    ///
    /// Return: the plugin's sort map (sort value → human title); merged
    /// over the built-in map. The default (`{"": ""}`) is the empty map.
    async fn hook_headlines_custom_sort_map(&self, _host: &mut HostCtx) -> Value {
        serde_json::json!({ "": "" })
    }

    /// `hook_headlines_custom_sort_override($order)` — override headline
    /// sorting; returns `(query, skip_first_id)`.
    /// `HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE`.
    /// `hook_headlines_custom_sort_override($order)` — override the built-in
    /// custom-sort query. `HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE`.
    ///
    /// PHP `Feeds.php:2359` (`_order_to_override_query`) runs
    /// `chain_hooks_callback(..., $order)` before building the custom-sort
    /// SQL; the callback takes `$result = [query, skip_first_id]` and
    /// *stops at the first non-empty query*. The port runs the same chain in
    /// `headlines.rs` (`order_to_override_query`, used by view, API
    /// getHeadlines, and syndication) with the same short-circuit.
    ///
    /// Parameters: `order` str — the requested sort specification (the
    /// client's `custom_sort` value).
    ///
    /// Return: `[query, skip_first_id]` — `query` str ("" = no override)
    /// and `skip_first_id` bool (skip the built-in `first_id` handling).
    /// The default `["", false]` declines.
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
    /// `hook_post_logout($login, $user_id)` — user just logged out.
    /// `HOOK_POST_LOGOUT`.
    ///
    /// PHP `Handler_Public.php:293` (`logout`) runs
    /// `run_hooks_callback(HOOK_POST_LOGOUT, cb, $login, $user_id)`: the
    /// first non-empty `[0]` (URL-validated) becomes the post-logout
    /// redirect target, otherwise the default. The port runs the same chain
    /// in `http.rs` (the logout route) with the same first-non-empty-wins
    /// semantics.
    ///
    /// Parameters:
    /// - `login` str — the logged-out user's login name
    /// - `user_id` int — the user id
    ///
    /// Return: `[redirect_url, …]` — `[0]` str is the post-logout redirect
    /// target ("" = no redirect; the default page loads).
    async fn hook_post_logout(&self, _host: &mut HostCtx, _login: &str, _user_id: i32) -> Value {
        serde_json::json!([""])
    }

    /// `hook_loginform_additional_buttons()` — buttons to the right of the
    /// default Login button. `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`.
    /// `hook_loginform_additional_buttons()` — extra buttons on the login form.
    /// `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`.
    ///
    /// PHP `login_form.php:210` runs `run_hooks(HOOK_LOGINFORM_ADDITIONAL_BUTTONS)`
    /// with the *system* plugins host (PHP's
    /// `load_all(KIND_SYSTEM, 0, true)`); the echo hooks' output lands in
    /// the login form. The port runs the same chain in `http.rs` (both the
    /// form-render and POST login routes) with a system-plugins host; the
    /// output lands to the right of the Login button.
    ///
    /// Parameters: none.
    ///
    /// Return: nothing (`()`); the hook echoes its HTML through the host
    /// (`host.output`).
    async fn hook_loginform_additional_buttons(&self, _host: &mut HostCtx) -> () {}

    /// `hook_validate_session()` — `false` marks the session invalid, `true`
    /// cascades to the next handler. `HOOK_VALIDATE_SESSION`.
    /// `hook_validate_session()` — validate/extend a session.
    /// `HOOK_VALIDATE_SESSION`.
    ///
    /// PHP `Sessions.php:144` (`validate_session`) runs
    /// `chain_hooks_callback(HOOK_VALIDATE_SESSION, cb)` with no arguments
    /// after the default checks: `$hook_result = $result`, and the chain
    /// *stops at the first `false`* — the chain's final result is the
    /// return value (default `true` when no hook is registered). The port
    /// runs the same chain in `session.rs::validate_session` with the
    /// same last-result-wins / stop-at-false semantics; the final `bool`
    /// is the validation result.
    ///
    /// Parameters: none.
    ///
    /// Return: `true` — the session is valid (continue); `false` —
    /// invalid (the chain stops and the session is rejected). The default
    /// `false` declines (treated as "no opinion" by the port's chain; PHP
    /// would reject — a plugin implementing this hook must always
    /// return an explicit verdict).
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
