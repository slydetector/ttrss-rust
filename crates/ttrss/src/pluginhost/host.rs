//! The plugin host — port of `tt-rss/classes/PluginHost.php` (M7.1).
//!
//! Two types, mirroring PHP's singleton split:
//!
//! - [`PluginHost`] — the shared registry: plugin instances, hooks by type +
//!   priority, handlers/commands/API methods/filter actions/scheduled tasks.
//!   Built once at startup ([`PluginHost::startup`]); all plugins are
//!   registered and `init()`-ed up front (PHP runs `init()` per host load;
//!   the PHP singleton is per-request, so once-per-request there vs
//!   once-per-process here — `init()` is registration-only, so the
//!   observable behavior is the same).
//! - [`HostCtx`] — the per-request/per-owner *view* (PHP's
//!   `getInstance()`/`load(KIND_USER, $uid)` layering): which plugins are
//!   **active** for this context, the owner uid, the profile, the access
//!   level, the DB pool, and the per-request special feeds. Every hook
//!   invocation receives `&mut HostCtx` — the Rust analogue of the `$host`
//!   the PHP plugins store in `init()`.
//!
//! **Gating (PHP's `KIND_*` loads).** PHP accumulates plugins onto the host
//! per load call: `KIND_ALL` registers everything in the list, `KIND_SYSTEM`
//! only `is_system` plugins, `KIND_USER` only non-system plugins, and a
//! plugin already registered is skipped. The port precomputes the resulting
//! active set per context:
//!
//! - [`HostCtx::for_user`] — `init_plugins()` (`TTRSS_PLUGINS`, `KIND_ALL`)
//!   + `UserHelper::load_user_plugins` / the feed-update path
//!     (`_ENABLED_PLUGINS`, `KIND_USER`, owner uid): active = in the system
//!     list, or (in the user list AND not a system plugin).
//! - [`HostCtx::system_only`] — `Cache_Local` (`KIND_SYSTEM`): active = in
//!   the system list AND a system plugin.
//! - [`HostCtx::single`] — `Handler_Public` public-method dispatch
//!   (`load($plugin_name, KIND_ALL, 0)`): one plugin by name.
//!
//! **Run modes (PHP's four).** `run_hooks` / `run_hooks_until` /
//! `run_hooks_callback` / `chain_hooks_callback` are the four generic
//! runners here ([`HostCtx::run_hooks`], [`HostCtx::run_hooks_until`],
//! [`HostCtx::run_hooks_callback`], [`HostCtx::chain_hooks_callback`]):
//! identical ordering (priority ascending, insertion order within a
//! priority — PHP's `ksort`) and short-circuit semantics; PHP's dynamic
//! `$plugin->$hook(...)` call becomes a per-hook closure the call site
//! supplies (M7.4+ wires the call sites).
//!
//! **Deliberate deviations from PHP:**
//! - No `$_SESSION["plugin_blacklist"]` / `safe_mode` machinery (a recovery
//!   hack for PHP fatal errors, which Rust does not have) and no
//!   `require`/`class_exists` file loading (static compilation, locked
//!   decision #1).
//! - `add_hook` has no `method_exists` guard: every hook exists as a trait
//!   method with a default, so the guard would be dead code.
//! - PHP's per-invocation `try/catch` → `user_error` is not replicated (see
//!   the module docs in [`super::plugin`]).
//! - Scheduled-task names (PHP `Class:task[:uid]`, class in original case)
//!   are resolved at schedule time, not `init` time — the port's `init` runs
//!   without an owner uid (see [`PluginHost::add_scheduled_task`]).
//! - `get_feed_handler` does not check PHP's `IVirtualFeed` interface
//!   (no first-party plugin implements it; no such trait in the port yet).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde_json::Value;
use sqlx::PgPool;

use crate::render::urlencode;
use crate::core::config::Config;

use super::plugin::{BoxFuture, Plugin};

/// `PluginHost::API_VERSION` (classes/PluginHost.php:40) — the minimum
/// plugin API version the host will load.
pub const API_VERSION: i32 = 2;

/// `PluginHost::PUBLIC_METHOD_DELIMITER` (classes/PluginHost.php:42).
pub const PUBLIC_METHOD_DELIMITER: &str = "--";

/// `Feeds::CATEGORY_SPECIAL` (classes/Feeds.php:35) — the only category id
/// accepted by [`HostCtx::add_feed`].
pub const CATEGORY_SPECIAL: i32 = -1;

/// `functions.php:3` — base index for plugin virtual feed ids.
pub const PLUGIN_FEED_BASE_INDEX: i64 = -128;

/// `PluginHost::pfeed_to_feed_id` (classes/PluginHost.php:803).
pub fn pfeed_to_feed_id(pfeed: i64) -> i64 {
    PLUGIN_FEED_BASE_INDEX - 1 - pfeed.abs()
}

/// `PluginHost::feed_to_pfeed_id` (classes/PluginHost.php:811).
pub fn feed_to_pfeed_id(feed: i64) -> i64 {
    PLUGIN_FEED_BASE_INDEX - 1 + feed.abs()
}

/// A registered CLI command (PHP `commands[]` entry).
#[derive(Debug)]
pub struct Command {
    pub description: String,
    pub suffix: String,
    pub arghelp: String,
    /// Index into [`PluginHost::plugins`].
    pub plugin: usize,
}

/// A filter action a plugin contributes (PHP `plugin_actions[]` entry).
#[derive(Debug, Clone)]
pub struct FilterAction {
    pub action: String,
    pub description: String,
}

/// A plugin-provided (virtual) feed (PHP `special_feeds[]` entry).
#[derive(Debug)]
pub struct SpecialFeed {
    pub id: i32,
    pub title: String,
    pub icon: String,
    /// Index into [`PluginHost::plugins`].
    pub sender: usize,
}

/// A scheduled task a plugin registered (PHP `add_scheduled_task`).
///
/// Execution (the `Scheduler` integration) lands with the first plugin that
/// uses one (M7.12); this increment stores the registration only.
pub struct ScheduledTask {
    /// The bare task name as registered (PHP's `$task_name` argument).
    pub task_name: String,
    pub cron: String,
    /// Index into [`PluginHost::plugins`].
    pub plugin: usize,
    pub callback: Box<dyn Fn(PgPool, Option<i32>, Config) -> BoxFuture<'static, ()> + Send + Sync>,
}

/// A scheduled task's full name (PHP `add_scheduled_task` naming):
/// system plugin → `Class:task`, user plugin → `Class:task:uid` (PHP's
/// `$this->owner_uid`, `0` when unset).
pub fn scheduled_task_name(plugin: &dyn Plugin, task_name: &str, owner_uid: Option<i32>) -> String {
    if plugin.about().system {
        format!("{}:{}", plugin.class_name(), task_name)
    } else {
        format!("{}:{}:{}", plugin.class_name(), task_name, owner_uid.unwrap_or(0))
    }
}

/// `PluginHost::get_method_url` (classes/PluginHost.php:845).
pub fn method_url(self_url: &str, plugin_name: &str, method: &str, params: &[(String, String)]) -> String {
    let mut q = format!(
        "op=pluginhandler&plugin={}&method={}",
        urlencode(plugin_name),
        urlencode(method)
    );
    for (k, v) in params {
        q.push_str(&format!("&{}={}", urlencode(k), urlencode(v)));
    }
    format!("{}/backend.php?{}", self_url.trim_end_matches('/'), q)
}

/// `PluginHost::get_public_method_url` (classes/PluginHost.php:877):
/// `public.php?op=<name>--<method>&params`, or `None` when the method is not
/// public (PHP logs `user_error` in that case).
pub fn public_method_url(
    self_url: &str,
    plugin: &dyn Plugin,
    method: &str,
    params: &[(String, String)],
) -> Option<String> {
    if !plugin.is_public_method(method) {
        return None;
    }
    let mut q = format!(
        "op={}",
        urlencode(&format!("{}{}{}", plugin.name(), PUBLIC_METHOD_DELIMITER, method))
    );
    for (k, v) in params {
        q.push_str(&format!("&{}={}", urlencode(k), urlencode(v)));
    }
    Some(format!("{}/public.php?{}", self_url.trim_end_matches('/'), q))
}

/// The shared plugin registry (PHP's `PluginHost` singleton, minus the
/// per-request/per-owner state that lives on [`HostCtx`]).
///
/// Built once at startup; shared across worker threads via `Arc`.
pub struct PluginHost {
    plugins: Vec<Box<dyn Plugin>>,
    /// Lowercase plugin name → index (PHP's `plugins[]` map).
    by_name: HashMap<String, usize>,
    /// Hook name → priority → plugin indices (PHP's `hooks[]`; priority
    /// keys are sorted — PHP's `ksort`).
    hooks: HashMap<&'static str, BTreeMap<i32, Vec<usize>>>,
    /// Handler → method → plugin index (PHP's `handlers[]`; system plugins
    /// only).
    handlers: HashMap<String, HashMap<String, usize>>,
    /// `"-<command>"` → details (PHP's `commands[]`).
    commands: BTreeMap<String, Command>,
    /// API method name → plugin index (PHP's `api_methods[]`; system plugins
    /// only).
    api_methods: HashMap<String, usize>,
    /// Plugin index → filter actions (PHP's `plugin_actions[]`, keyed by
    /// sender class name).
    filter_actions: HashMap<usize, Vec<FilterAction>>,
    scheduled_tasks: Vec<ScheduledTask>,
    /// The plugin currently being `init()`-ed (target of registration
    /// calls) — the Rust stand-in for PHP's explicit `$sender` argument.
    current: Option<usize>,
    /// Whether the plugin currently being `init()`-ed is a system plugin
    /// (PHP's `is_system($sender)`; computed in `run_init`, where the plugin
    /// is at hand — it is out of `plugins` while `init()` runs).
    current_is_system: bool,
}

impl PluginHost {
    /// Register a plugin (PHP's `load()` per-class step, minus file
    /// loading): skip if the name is taken, skip if the API version is below
    /// [`API_VERSION`], then run `init()` (registration only).
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> bool {
        let name = plugin.name().to_lowercase();
        if self.by_name.contains_key(&name) {
            return false;
        }
        if plugin.api_version() < API_VERSION {
            tracing::warn!(
                plugin = %name,
                version = plugin.api_version(),
                "plugin is not compatible with current API version; not loading it"
            );
            return false;
        }
        let idx = self.plugins.len();
        self.by_name.insert(name, idx);
        self.plugins.push(plugin);
        self.run_init(idx);
        true
    }

    /// `PluginHost::startup` (the port's one-time equivalent of PHP's
    /// per-host `load(TTRSS_PLUGINS, KIND_ALL)` + per-request init):
    /// instantiate the statically registered plugins, check their API
    /// version, and run `init()` once each.
    pub fn startup(registry: &[fn() -> Box<dyn Plugin>]) -> Self {
        let mut host = Self::new();
        for factory in registry {
            host.register(factory());
        }
        host
    }

    fn run_init(&mut self, idx: usize) {
        let prev = self.current.replace(idx);
        // `init` needs `&mut self` (the registration API), but the plugin
        // lives inside `self.plugins` — pop it, init, push it back (it was
        // just pushed, so the index is unchanged).
        let plugin = self.plugins.pop().expect("plugin under init");
        self.current_is_system = Self::is_system(plugin.as_ref());
        plugin.init(self);
        self.plugins.push(plugin);
        debug_assert_eq!(self.plugins.len() - 1, idx);
        self.current = prev;
    }

    // -- plugin lookups -----------------------------------------------------

    /// All registered plugins, in registration order (PHP
    /// `get_plugin_names`/`get_plugins`).
    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }

    /// The plugin at `idx` (registry-internal; used by the run modes).
    pub(crate) fn plugin(&self, idx: usize) -> &dyn Plugin {
        self.plugins[idx].as_ref()
    }

    /// `PluginHost::get_plugin($name)` — by lowercase name.
    pub fn get_plugin(&self, name: &str) -> Option<&dyn Plugin> {
        self.by_name.get(&name.to_lowercase()).map(|&i| &*self.plugins[i])
    }

    /// `PluginHost::is_system($plugin)` — `about()[3] === true`.
    pub fn is_system(plugin: &dyn Plugin) -> bool {
        plugin.about().system
    }

    // -- registration API (called from `init()`) -----------------------------

    /// `PluginHost::add_hook($type, $sender, $priority = 50)` — the sender is
    /// the plugin whose `init()` is running.
    pub fn add_hook(&mut self, hook: &'static str, priority: i32) {
        let Some(idx) = self.current else {
            tracing::error!(plugin_hook = %hook, "add_hook outside of plugin init");
            return;
        };
        self.hooks.entry(hook).or_default().entry(priority).or_default().push(idx);
    }

    /// Convenience for the PHP default priority (50).
    pub fn add_hook_default(&mut self, hook: &'static str) {
        self.add_hook(hook, 50);
    }

    /// `PluginHost::del_hook($type, $sender)`.
    pub fn del_hook(&mut self, hook: &'static str) {
        let Some(idx) = self.current else {
            return;
        };
        if let Some(map) = self.hooks.get_mut(hook) {
            for list in map.values_mut() {
                list.retain(|&i| i != idx);
            }
        }
    }

    /// `PluginHost::add_handler($handler, $method, $sender)` — **system
    /// plugins only** (PHP: only system plugins may modify routing).
    pub fn add_handler(&mut self, handler: &str, method: &str) {
        let Some(idx) = self.current else {
            return;
        };
        if !self.current_is_system {
            return;
        }
        self.handlers
            .entry(handler.replace('-', "_").to_lowercase())
            .or_default()
            .insert(method.to_lowercase(), idx);
    }

    /// `PluginHost::del_handler($handler, $method, $sender)` — system
    /// plugins only.
    pub fn del_handler(&mut self, handler: &str, method: &str) {
        if let Some(methods) = self.handlers.get_mut(&handler.replace('-', "_").to_lowercase()) {
            methods.remove(&method.to_lowercase());
        }
    }

    /// `PluginHost::lookup_handler($handler, $method)` — the `"*"` wildcard
    /// method wins over the specific one (PHP's `$handlers[$handler]["*"] ??
    /// $handlers[$handler][$method]`).
    pub fn lookup_handler(&self, handler: &str, method: &str) -> Option<&dyn Plugin> {
        let methods = self.handlers.get(&handler.replace('-', "_").to_lowercase())?;
        let idx = match methods.get("*") {
            Some(i) => *i,
            None => *methods.get(&method.to_lowercase())?,
        };
        Some(self.plugin(idx))
    }
    /// `PluginHost::add_command($command, $description, $sender, $suffix,
    /// $arghelp)`.
    pub fn add_command(&mut self, command: &str, description: &str, suffix: &str, arghelp: &str) {
        let Some(idx) = self.current else {
            return;
        };
        let key = format!("-{}", command.replace('-', "_").to_lowercase());
        self.commands.insert(
            key,
            Command {
                description: description.to_string(),
                suffix: suffix.to_string(),
                arghelp: arghelp.to_string(),
                plugin: idx,
            },
        );
    }

    /// `PluginHost::del_command($command)`.
    pub fn del_command(&mut self, command: &str) {
        self.commands.remove(&format!("-{}", command.to_lowercase()));
    }

    /// `PluginHost::lookup_command($command)` (PHP lowercases; it does not
    /// convert `-`/`_` — callers pass the underscore form).
    pub fn lookup_command(&self, command: &str) -> Option<&dyn Plugin> {
        self.commands
            .get(&format!("-{}", command.to_lowercase()))
            .map(|c| &*self.plugins[c.plugin])
    }

    /// `PluginHost::get_commands()`.
    pub fn get_commands(&self) -> &BTreeMap<String, Command> {
        &self.commands
    }
    /// `PluginHost::add_api_method($name, $sender)` — **system plugins
    /// only**.
    pub fn add_api_method(&mut self, name: &str) {
        let Some(idx) = self.current else {
            return;
        };
        if !self.current_is_system {
            return;
        }
        self.api_methods.insert(name.to_lowercase(), idx);
    }

    /// `PluginHost::get_api_method($name)`.
    pub fn get_api_method(&self, name: &str) -> Option<&dyn Plugin> {
        self.api_methods
            .get(&name.to_lowercase())
            .map(|&i| &*self.plugins[i])
    }

    /// `PluginHost::add_filter_action($sender, $action_name, $action_desc)`.
    pub fn add_filter_action(&mut self, action: &str, description: &str) {
        let Some(idx) = self.current else {
            return;
        };
        self.filter_actions
            .entry(idx)
            .or_default()
            .push(FilterAction {
                action: action.to_string(),
                description: description.to_string(),
            });
    }

    /// `PluginHost::get_filter_actions()` — plugin index → actions (PHP keys
    /// the map by sender class name).
    pub fn get_filter_actions(&self) -> &HashMap<usize, Vec<FilterAction>> {
        &self.filter_actions
    }

    /// `PluginHost::add_scheduled_task($sender, $task_name, $cron, $callback)`.
    ///
    /// The full task name (PHP `Class:task[:uid]`) is resolved at schedule
    /// time via [`scheduled_task_name`], since the port's `init` runs
    /// without an owner uid (see module docs).
    pub fn add_scheduled_task(
        &mut self,
        task_name: &str,
        cron: &str,
        callback: Box<dyn Fn(PgPool, Option<i32>, Config) -> BoxFuture<'static, ()> + Send + Sync>,
    ) {
        let Some(idx) = self.current else {
            return;
        };
        self.scheduled_tasks.push(ScheduledTask {
            task_name: task_name.to_string(),
            cron: cron.to_string(),
            plugin: idx,
            callback,
        });
    }

    /// The registered scheduled tasks (execution is the M6c.4 scheduler's
    /// job, M7.12).
    pub fn scheduled_tasks(&self) -> &[ScheduledTask] {
        &self.scheduled_tasks
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    /// An empty registry (tests; production uses [`PluginHost::startup`]).
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            by_name: HashMap::new(),
            hooks: HashMap::new(),
            handlers: HashMap::new(),
            commands: BTreeMap::new(),
            api_methods: HashMap::new(),
            filter_actions: HashMap::new(),
            scheduled_tasks: Vec::new(),
            current: None,
            current_is_system: false,
        }
    }
}

/// The per-request/per-owner plugin context (PHP's loaded `PluginHost`
/// state for one owner): the active plugin set + owner/profile/access
/// level + DB pool + per-request special feeds.
///
/// Built per request (web), per feed-update task (the feed owner, default
/// profile), or per public-method dispatch (one plugin, owner 0) — see the
/// constructors.
pub struct HostCtx<'a> {
    host: &'a PluginHost,
    /// Active flag per plugin index (see the gating rules in the module
    /// docs).
    active: Vec<bool>,
    /// The context's owner uid (PHP's `$this->owner_uid`): the session user
    /// on the web path, the feed owner on the update path, `None`/`0`
    /// otherwise. Storage (M7.2) and scheduled-task naming key off it.
    pub owner_uid: Option<i32>,
    /// The active preference profile (`None` = default profile; the update
    /// path always uses `None`, mirroring PHP's non-profile-scoped
    /// `_ENABLED_PLUGINS` read there).
    pub profile: Option<i32>,
    /// The session's access level (`0` outside a web session).
    pub access_level: i32,
    pub pool: &'a PgPool,
    /// Special (plugin-provided) feeds — per-request in PHP (the singleton
    /// is per-request), so per-context here.
    special_feeds: Vec<SpecialFeed>,
    /// The per-host plugin storage map (PHP's `$this->storage`): plugin
    /// class name → the owner's data map. Lazy-loaded by the `get` side;
    /// see `storage.rs` (M7.2).
    pub(crate) storage: std::collections::HashMap<String, crate::core::phpser::Value>,
    /// PHP's `$this->data_loaded` guard.
    pub(crate) data_loaded: bool,
    /// `Config::get_self_url()` — the context's base URL (plugins build
    /// absolute asset URLs off it, e.g. `nsfw`'s placeholder image).
    pub self_url: String,
    /// The base media-cache directory (PHP `Config::CACHE_DIR`), for the
    /// `DiskCache`-using plugins (e.g. `cache_starred_images`). Set via
    /// [`HostCtx::with_cache_dir`]; empty on the pre-plugin baseline.
    pub cache_dir: PathBuf,
    /// Hook output buffer (PHP's `ob_start()` around the hook chain): hooks
    /// that *print* (e.g. `hook_prefs_tab`) append here; the caller of the
    /// chain collects/clears it. M7.10 (the prefs page) is the first runner.
    pub output: String,
    /// `Config::fetch_extended_url_filtering` — threaded into the
    /// `bookmarklets` plugin's `subscribe` public method (the
    /// `Feeds::_subscribe` call).
    pub fetch_filtering: bool,
}

impl<'a> HostCtx<'a> {
    /// A view with no plugins active (the pre-plugin baseline).
    pub fn empty(host: &'a PluginHost, pool: &'a PgPool) -> Self {
        Self::with_active(
            host,
            pool,
            vec![false; host.plugins.len()],
            None,
            None,
            0,
        )
    }

    /// Web-request / per-owner context: `TTRSS_PLUGINS` (system) + the
    /// owner's `_ENABLED_PLUGINS` (user, `KIND_USER`).
    pub fn for_user(
        host: &'a PluginHost,
        pool: &'a PgPool,
        owner_uid: Option<i32>,
        profile: Option<i32>,
        access_level: i32,
        system: &[&str],
        user: &[&str],
    ) -> Self {
        let system: Vec<&str> = system.iter().map(|s| s.trim()).collect();
        let user: Vec<&str> = user.iter().map(|s| s.trim()).collect();
        let active = host
            .plugins
            .iter()
            .map(|p| {
                let name = p.name();
                system.contains(&name) || (user.contains(&name) && !PluginHost::is_system(p.as_ref()))
            })
            .collect();
        Self::with_active(host, pool, active, owner_uid, profile, access_level)
    }

    /// `Cache_Local`'s host: system plugins only (`KIND_SYSTEM`).
    pub fn system_only(host: &'a PluginHost, pool: &'a PgPool, system: &[&str]) -> Self {
        let system: Vec<&str> = system.iter().map(|s| s.trim()).collect();
        let active = host
            .plugins
            .iter()
            .map(|p| system.contains(&p.name()) && PluginHost::is_system(p.as_ref()))
            .collect();
        Self::with_active(host, pool, active, None, None, 0)
    }

    /// `Handler_Public`'s public-method host: one plugin by name
    /// (`load($plugin_name, KIND_ALL, 0)`).
    pub fn single(host: &'a PluginHost, pool: &'a PgPool, name: &str) -> Self {
        let name = name.trim().to_lowercase();
        let active = host
            .plugins
            .iter()
            .map(|p| p.name() == name)
            .collect();
        Self::with_active(host, pool, active, Some(0), None, 0)
    }

    fn with_active(
        host: &'a PluginHost,
        pool: &'a PgPool,
        active: Vec<bool>,
        owner_uid: Option<i32>,
        profile: Option<i32>,
        access_level: i32,
    ) -> Self {
        Self {
            host,
            active,
            owner_uid,
            profile,
            access_level,
            pool,
            special_feeds: Vec::new(),
            storage: std::collections::HashMap::new(),
            data_loaded: false,
            self_url: String::new(),
            cache_dir: PathBuf::new(),
            output: String::new(),
            fetch_filtering: false,
        }
    }

    /// `Config::set_self_url`-equivalent for the request path (the PHP
    /// singleton reads the config globally; the port hands it in once per
    /// context).
    #[must_use]
    pub fn with_self_url(mut self, self_url: impl Into<String>) -> Self {
        self.self_url = self_url.into();
        self
    }

    /// `Config::CACHE_DIR`-equivalent: the base media-cache directory the
    /// `DiskCache`-using plugins (e.g. `cache_starred_images`) resolve
    /// instances against.
    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = cache_dir.into();
        self
    }

    /// `Config::fetch_extended_url_filtering`-equivalent for the request
    /// path (the PHP singleton reads the config globally; the port hands it
    /// in once per context).
    #[must_use]
    pub fn with_fetch_filtering(mut self, fetch_filtering: bool) -> Self {
        self.fetch_filtering = fetch_filtering;
        self
    }
    /// The shared registry.
    pub fn host(&self) -> &'a PluginHost {
        self.host
    }

    /// Whether the plugin at `idx` is active in this context.
    pub fn is_active(&self, idx: usize) -> bool {
        self.active.get(idx).copied().unwrap_or(false)
    }

    /// `PluginHost::get_plugin($name)` restricted to this context's active
    /// set: PHP's per-request host only *holds* loaded plugins, so a plugin
    /// that is registered but not active for the owner (system list /
    /// `_ENABLED_PLUGINS`) is an unknown plugin on the dispatch paths.
    pub fn get_plugin(&self, name: &str) -> Option<&'a dyn Plugin> {
        let &i = self.host.by_name.get(&name.to_lowercase())?;
        if self.active.get(i).copied().unwrap_or(false) {
            Some(self.host.plugins[i].as_ref())
        } else {
            None
        }
    }

    /// The active plugins, in registration order.
    pub fn active_plugins(&self) -> impl Iterator<Item = &dyn Plugin> + '_ {
        self.host
            .plugins
            .iter()
            .enumerate()
            .filter(|(i, _)| self.is_active(*i))
            .map(|(_, p)| p.as_ref())
    }

    /// The plugins registered for `hook`, in invocation order (priority
    /// ascending — PHP's `ksort`; insertion order within a priority),
    /// restricted to the active set.
    pub fn hook_order(&self, hook: &str) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(map) = self.host.hooks.get(hook) {
            for list in map.values() {
                for &idx in list {
                    if self.is_active(idx) {
                        out.push(idx);
                    }
                }
            }
        }
        out
    }

    // -- the four run modes (PHP's `run_hooks*`) ------------------------------
    //
    // The invoke closures return a `BoxFuture` borrowing the per-call
    // `plugin`/`ctx` arguments (`for<'x>`), so a closure's *captures* must
    // be owned or `'static`-ish: clone per call (`let v = v.clone();` before
    // the `async move`) or pass references the data outlives.
    /// `run_hooks($hook, ...$args)` — invoke every active plugin's hook in
    /// priority order; results ignored.
    pub async fn run_hooks<F>(&mut self, hook: &str, mut invoke: F)
    where
        for<'x> F: FnMut(&'x dyn Plugin, &'x mut HostCtx<'a>) -> BoxFuture<'x, ()>,
    {
        let host = self.host;
        for idx in self.hook_order(hook) {
            invoke(host.plugin(idx), self).await;
        }
    }

    /// `run_hooks_until($hook, $check, ...$args)` — invoke in priority order
    /// until a plugin's result satisfies `check` (PHP: `$result == $check`);
    /// returns whether one did.
    pub async fn run_hooks_until<R, C, F>(
        &mut self,
        hook: &str,
        check: C,
        invoke: F,
    ) -> bool
    where
        C: Fn(&R) -> bool,
        for<'x> F: Fn(&'x dyn Plugin, &'x mut HostCtx<'a>) -> BoxFuture<'x, R>,
    {
        let host = self.host;
        for idx in self.hook_order(hook) {
            if check(&invoke(host.plugin(idx), self).await) {
                return true;
            }
        }
        false
    }

    /// `run_hooks_callback($hook, $callback, ...$args)` — invoke in priority
    /// order; stop when `stop(&result, plugin)` returns `true`.
    pub async fn run_hooks_callback<R, S, F>(
        &mut self,
        hook: &str,
        invoke: F,
        mut stop: S,
    )
    where
        S: FnMut(&R, &dyn Plugin) -> bool,
        for<'x> F: Fn(&'x dyn Plugin, &'x mut HostCtx<'a>) -> BoxFuture<'x, R>,
    {
        let host = self.host;
        for idx in self.hook_order(hook) {
            if stop(&invoke(host.plugin(idx), self).await, host.plugin(idx)) {
                break;
            }
        }
    }

    /// `chain_hooks_callback($hook, $callback, &...$args)` — like
    /// [`HostCtx::run_hooks_callback`], but threads `state` (PHP's
    /// by-reference arguments) mutably through each invocation.
    pub async fn chain_hooks_callback<S, R, St, F>(
        &mut self,
        hook: &str,
        state: &mut S,
        invoke: F,
        mut stop: St,
    )
    where
        St: FnMut(&R, &dyn Plugin) -> bool,
        for<'x> F: Fn(&'x dyn Plugin, &'x mut HostCtx<'a>, &'x mut S) -> BoxFuture<'x, R>,
    {
        let host = self.host;
        for idx in self.hook_order(hook) {
            if stop(&invoke(host.plugin(idx), self, state).await, host.plugin(idx)) {
                break;
            }
        }
    }

    // -- CLI commands (PHP `run_commands`) -------------------------------------

    /// `PluginHost::run_commands($args)` — for each registered command, if
    /// `$args` carries the dashed key, invoke the plugin's method of the
    /// undashed name (PHP: `$class->$command($args)`). No first-party plugin
    /// registers a command in the frozen reference, so this is a no-op for
    /// the stock set.
    pub async fn run_commands(&mut self, args: &Value) {
        // Collect up front: the `&mut self` for `invoke_method` may not
        // overlap the `self.host.get_commands()` borrow.
        let cmds: Vec<(String, usize)> = self
            .host
            .get_commands()
            .iter()
            .map(|(k, c)| (k.clone(), c.plugin))
            .collect();
        for (key, idx) in cmds {
            if args.get(key.as_str()).is_some() {
                let method = key.trim_start_matches('-').to_string();
                let plugin = self.host.plugin(idx);
                if let Err(e) = plugin.invoke_method(self, &method, args).await {
                    tracing::warn!(command = %method, error = %e, "plugin command failed");
                }
            }
        }
    }

    // -- special (plugin-provided) feeds --------------------------------------

    /// `PluginHost::add_feed($cat_id, $title, $icon, $sender)` — only
    /// [`CATEGORY_SPECIAL`] is supported.
    pub fn add_feed(&mut self, cat_id: i32, title: &str, icon: &str, plugin: &dyn Plugin) -> Option<i32> {
        if cat_id != CATEGORY_SPECIAL {
            return None;
        }
        let idx = self
            .host
            .plugins
            .iter()
            .position(|p| std::ptr::eq(p.as_ref(), plugin))?;
        let id = self.special_feeds.len() as i32;
        self.special_feeds.push(SpecialFeed {
            id,
            title: title.to_string(),
            icon: icon.to_string(),
            sender: idx,
        });
        Some(id)
    }

    /// `PluginHost::get_feeds($cat_id)` — only [`CATEGORY_SPECIAL`].
    pub fn get_feeds(&self, cat_id: i32) -> &[SpecialFeed] {
        if cat_id == CATEGORY_SPECIAL {
            &self.special_feeds
        } else {
            &[]
        }
    }

    /// `PluginHost::get_feed_handler($pfeed_id)` — the plugin handling a
    /// virtual feed (see module docs for the omitted `IVirtualFeed` check).
    pub fn get_feed_handler(&self, pfeed_id: i32) -> Option<&dyn Plugin> {
        let feed = self.special_feeds.iter().find(|f| f.id == pfeed_id)?;
        Some(self.host.plugin(feed.sender))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::pluginhost::PluginError;
    use crate::pluginhost::plugin::{About, MethodOutput, Plugin};
    use crate::pluginhost::HOOK_ARTICLE_BUTTON;

    /// A test plugin: registers `HOOK_ARTICLE_BUTTON` at `prio` in `init`,
    /// records every hook invocation, and returns its name from the hook.
    struct TestPlugin {
        name: &'static str,
        class: &'static str,
        system: bool,
        api: i32,
        prio: i32,
        handler: Option<(&'static str, &'static str)>, // (handler, method) to register
        del: bool, // init registers the hook, then removes it again
        command: bool,
        api_method: bool,
        public: bool,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl TestPlugin {
        fn new(name: &'static str, system: bool, calls: &Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                name,
                class: name,
                system,
                api: 2,
                prio: 50,
                handler: None,
                del: false,
                command: false,
                api_method: false,
                public: false,
                calls: calls.clone(),
            }
        }
        fn boxit(self) -> Box<dyn Plugin> {
            Box::new(self)
        }
    }

    impl Plugin for TestPlugin {
        fn name(&self) -> &'static str {
            self.name
        }
        fn class_name(&self) -> &'static str {
            self.class
        }
        fn about(&self) -> About {
            let mut a = About::new("test", "tester");
            a.system = self.system;
            a
        }
        fn api_version(&self) -> i32 {
            self.api
        }
        fn is_public_method(&self, method: &str) -> bool {
            self.public && method == "newkey"
        }
        fn init(&self, host: &mut PluginHost) {
            host.add_hook(HOOK_ARTICLE_BUTTON, self.prio);
            if self.del {
                host.del_hook(HOOK_ARTICLE_BUTTON);
            }
            if let Some((h, m)) = self.handler {
                host.add_handler(h, m);
            }
            if self.command {
                host.add_command("my-cmd", "desc", "", "");
            }
            if self.api_method {
                host.add_api_method("m");
            }
        }
        fn hook_article_button<'a, 'h: 'a>(
            &'a self,
            _host: &'a mut HostCtx<'h>,
            _line: &'a Value,
        ) -> BoxFuture<'a, String> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(self.name.to_string());
                self.name.to_string()
            })
        }
        fn invoke_method<'a, 'h: 'a>(
            &'a self,
            _host: &'a mut HostCtx<'h>,
            method: &'a str,
            _params: &'a Value,
        ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
            Box::pin(async move {
                if method == "my_cmd" {
                    Ok(MethodOutput::Json(serde_json::json!({ "called": self.name })))
                } else {
                    Err(PluginError::UnknownMethod(method.to_string()))
                }
            })
        }
    }

    fn calls() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn names(c: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        c.lock().unwrap().iter().cloned().collect()
    }

    /// A pool handle that never connects (the mechanics tests do no I/O).
    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap()
    }

    /// Three plugins A(50) / B(10) / C(50) — invocation order must be
    /// B, A, C (priority ascending, insertion order within a priority).
    fn host_abc() -> (PluginHost, Arc<Mutex<Vec<String>>>) {
        let c = calls();
        let a = TestPlugin::new("a", false, &c);
        let mut b = TestPlugin::new("b", false, &c);
        let cpl = TestPlugin::new("c", false, &c);
        b.prio = 10;
        let mut host = PluginHost::new();
        host.register(a.boxit());
        host.register(b.boxit());
        host.register(cpl.boxit());
        (host, c)
    }

    #[tokio::test]
    async fn priority_ordering_and_insertion() {
        let (host, _c) = host_abc();
        let pool = lazy_pool();
        let ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        let hostref = &host;
        let order: Vec<&str> = ctx
            .hook_order(HOOK_ARTICLE_BUTTON)
            .iter()
            .map(|&i| hostref.plugin(i).name())
            .collect();
        assert_eq!(order, vec!["b", "a", "c"]);
    }

    #[tokio::test]
    async fn hook_order_skips_inactive() {
        let (host, _c) = host_abc();
        let pool = lazy_pool();
        let ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "c"], &[]);
        let hostref = &host;
        let order: Vec<&str> = ctx
            .hook_order(HOOK_ARTICLE_BUTTON)
            .iter()
            .map(|&i| hostref.plugin(i).name())
            .collect();
        assert_eq!(order, vec!["a", "c"]);
    }

    #[tokio::test]
    async fn run_modes_until_callback_chain() {
        let (host, c) = host_abc();
        let pool = lazy_pool();
        // Shadowed as `&Value` (Copy) so the `Fn`/`FnMut` closures can hand
        // it to their `async move` blocks without moving the `Value`.
        let line = Value::Null;
        let line = &line;

        // run_hooks: all three, in order.
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        ctx
            .run_hooks(HOOK_ARTICLE_BUTTON, |_p, _h| {
                Box::pin(async move {
                    let _ = _p.hook_article_button(_h, line).await;
                })
            })
            .await;
        assert_eq!(names(&c), vec!["b", "a", "c"]);

        // run_hooks_until: stops at the first plugin whose result matches.
        c.lock().unwrap().clear();
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        let matched = ctx
            .run_hooks_until(
                HOOK_ARTICLE_BUTTON,
                |r: &String| r == "a",
                |p, h| {
                    Box::pin(async move { p.hook_article_button(h, line).await })
                },
            )
            .await;
        assert!(matched);
        assert_eq!(names(&c), vec!["b", "a"]);

        // No match → all invoked, false.
        c.lock().unwrap().clear();
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        let matched = ctx
            .run_hooks_until(
                HOOK_ARTICLE_BUTTON,
                |r: &String| r == "zzz",
                |p, h| {
                    Box::pin(async move { p.hook_article_button(h, line).await })
                },
            )
            .await;
        assert!(!matched);
        assert_eq!(names(&c), vec!["b", "a", "c"]);

        // run_hooks_callback: stops when the predicate holds.
        c.lock().unwrap().clear();
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        ctx
            .run_hooks_callback(
                HOOK_ARTICLE_BUTTON,
                |p, h| {
                    Box::pin(async move { p.hook_article_button(h, line).await })
                },
                |r: &String, _p| *r == "a",
            )
            .await;
        assert_eq!(names(&c), vec!["b", "a"]);

        // chain_hooks_callback: threads state through, stops mid-chain.
        c.lock().unwrap().clear();
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a", "b", "c"], &[]);
        let mut acc = String::new();
        ctx
            .chain_hooks_callback(
                HOOK_ARTICLE_BUTTON,
                &mut acc,
                |p, h, s| {
                    Box::pin(async move {
                        let r = p.hook_article_button(h, line).await;
                        s.push_str(r.as_str());
                        r
                    })
                },
                |r: &String, _p| *r == "a",
            )
            .await;
        assert_eq!(names(&c), vec!["b", "a"]);
        assert_eq!(acc, "ba");
    }

    #[tokio::test]
    async fn gating_for_user() {
        let c = calls();
        let s = TestPlugin::new("s", true, &c);
        let u = TestPlugin::new("u", false, &c);
        let mut host = PluginHost::new();
        host.register(s.boxit());
        host.register(u.boxit());
        let pool = lazy_pool();

        // System list [s] + user list [u]: both active.
        let ctx = HostCtx::for_user(&host, &pool, Some(1), None, 0, &["s"], &["u"]);
        assert_eq!(
            ctx.active_plugins().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["s", "u"]
        );
        assert_eq!(ctx.owner_uid, Some(1));

        // A system plugin not in the system list is never active (KIND_USER
        // skips system plugins).
        let ctx = HostCtx::for_user(&host, &pool, Some(1), None, 0, &[], &["s"]);
        assert!(ctx.active_plugins().count() == 0);

        // A user plugin in the system list is active (KIND_ALL registers it).
        let ctx = HostCtx::for_user(&host, &pool, Some(1), None, 0, &["s", "u"], &[]);
        assert_eq!(
            ctx.active_plugins().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["s", "u"]
        );

        // Whitespace is trimmed (PHP's array_map(trim, explode(...))).
        let ctx = HostCtx::for_user(&host, &pool, Some(1), None, 0, &[" s "], &["u"]);
        assert_eq!(
            ctx.active_plugins().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["s", "u"]
        );
    }

    #[tokio::test]
    async fn gating_system_only_and_single() {
        let c = calls();
        let s = TestPlugin::new("s", true, &c);
        let u = TestPlugin::new("u", false, &c);
        let mut host = PluginHost::new();
        host.register(s.boxit());
        host.register(u.boxit());
        let pool = lazy_pool();

        let ctx = HostCtx::system_only(&host, &pool, &["s", "u"]);
        assert_eq!(
            ctx.active_plugins().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["s"]
        );

        let ctx = HostCtx::single(&host, &pool, "u");
        assert_eq!(
            ctx.active_plugins().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["u"]
        );
        assert_eq!(ctx.owner_uid, Some(0));
    }

    #[test]
    fn api_version_and_duplicate_name_gates() {
        let c = calls();
        let mut old = TestPlugin::new("old", false, &c);
        old.api = 1; // below API_VERSION (2)
        let mut host = PluginHost::new();
        assert!(!host.register(old.boxit()));
        assert!(host.plugins().is_empty());

        let a = TestPlugin::new("dup", false, &c);
        let a2 = TestPlugin::new("dup", false, &c);
        assert!(host.register(a.boxit()));
        assert!(!host.register(a2.boxit()));
        assert_eq!(host.plugins().len(), 1);
    }

    #[test]
    fn handlers_system_only_and_wildcard() {
        let c = calls();
        let mut s = TestPlugin::new("s", true, &c);
        s.handler = Some(("my-handler", "*"));
        let mut s2 = TestPlugin::new("s2", true, &c);
        s2.handler = Some(("my-handler", "doit"));
        let mut u = TestPlugin::new("u", false, &c);
        u.handler = Some(("user-handler", "steal")); // non-system: must be ignored
        let mut host = PluginHost::new();
        host.register(s.boxit());
        host.register(s2.boxit());
        host.register(u.boxit());

        // The "*" wildcard wins over specific methods (PHP: $handlers[$h]["*"]
        // ?? $handlers[$h][$method]).
        assert_eq!(
            host.lookup_handler("my-handler", "anything").map(|p| p.name()),
            Some("s")
        );
        // Handler normalization: '-' → '_' and lowercase.
        assert_eq!(host.lookup_handler("MY-Handler", "DOIT").map(|p| p.name()), Some("s"));
        // A non-system plugin's add_handler is ignored (PHP's is_system gate).
        assert!(host.lookup_handler("user-handler", "steal").is_none());
        assert!(host.lookup_handler("nope", "x").is_none());
    }
    #[test]
    fn commands_and_api_methods_and_filter_actions() {
        let c = calls();
        let mut s = TestPlugin::new("s", true, &c);
        s.command = true;
        s.api_method = true;
        let mut u = TestPlugin::new("u", false, &c);
        u.command = true; // commands are not system-gated in PHP
        u.api_method = true; // API methods are system-gated
        let mut host = PluginHost::new();
        host.register(s.boxit());
        host.register(u.boxit());

        // Same command name: the later registration wins (PHP's map insert).
        assert_eq!(host.lookup_command("my_cmd").map(|p| p.name()), Some("u"));
        assert_eq!(host.get_commands().len(), 1);
        assert_eq!(host.get_commands().get("-my_cmd").unwrap().description, "desc");

        // Only the system plugin's API method is registered.
        assert_eq!(host.get_api_method("m").map(|p| p.name()), Some("s"));
    }

    #[test]
    fn pfeed_id_math() {
        assert_eq!(pfeed_to_feed_id(-5), -134);
        assert_eq!(pfeed_to_feed_id(5), -134); // abs()
        assert_eq!(feed_to_pfeed_id(-134), 5); // -129 + 134
        assert_eq!(feed_to_pfeed_id(134), 5);
    }

    #[test]
    fn method_urls() {
        let url = method_url(
            "http://example/",
            "nsfw",
            "set",
            &[("a".into(), "b c".into())],
        );
        assert_eq!(
            url,
            "http://example/backend.php?op=pluginhandler&plugin=nsfw&method=set&a=b+c"
        );

        let c = calls();
        let mut pubp = TestPlugin::new("share", false, &c);
        pubp.public = true;
        let privp = TestPlugin::new("note", false, &c);
        assert_eq!(
            public_method_url("http://example/", pubp.boxit().as_ref(), "newkey", &[]),
            Some("http://example/public.php?op=share--newkey".to_string())
        );
        assert_eq!(
            public_method_url("http://example/", privp.boxit().as_ref(), "newkey", &[]),
            None
        );
    }

    #[tokio::test]
    async fn special_feeds() {
        let c = calls();
        let p = TestPlugin::new("virt", false, &c);
        let mut host = PluginHost::new();
        host.register(p.boxit());
        let pool = lazy_pool();
        let mut ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["virt"], &[]);
        let plugin = host.get_plugin("virt").unwrap();

        assert!(ctx.add_feed(5, "t", "i", plugin).is_none()); // not CATEGORY_SPECIAL
        let id = ctx.add_feed(CATEGORY_SPECIAL, "Virtual", "icon", plugin).unwrap();
        assert_eq!(id, 0);
        assert_eq!(ctx.get_feeds(CATEGORY_SPECIAL).len(), 1);
        assert_eq!(ctx.get_feeds(CATEGORY_SPECIAL)[0].title, "Virtual");
        assert!(ctx.get_feeds(1).is_empty());
        assert_eq!(ctx.get_feed_handler(id).map(|p| p.name()), Some("virt"));
        assert!(ctx.get_feed_handler(9).is_none());
    }

    #[test]
    fn scheduled_task_naming() {
        let c = calls();
        let mut s = TestPlugin::new("s", true, &c);
        s.class = "System_Thing"; // original case, as PHP stores it
        let mut u = TestPlugin::new("u", false, &c);
        u.class = "User_Thing";
        let sb_box = s.boxit();
        let sb = sb_box.as_ref();
        let ub_box = u.boxit();
        let ub = ub_box.as_ref();
        assert_eq!(scheduled_task_name(sb, "cache", None), "System_Thing:cache");
        assert_eq!(scheduled_task_name(ub, "cache", Some(7)), "User_Thing:cache:7");
        assert_eq!(scheduled_task_name(ub, "cache", None), "User_Thing:cache:0");
    }

    #[tokio::test]
    async fn del_hook_removes() {
        let c = calls();
        let mut p = TestPlugin::new("a", false, &c);
        p.del = true;
        let mut host = PluginHost::new();
        host.register(p.boxit());
        assert_eq!(host.get_plugin("a").unwrap().name(), "a");
        let pool = lazy_pool();
        let ctx = HostCtx::for_user(&host, &pool, None, None, 0, &["a"], &[]);
        assert!(ctx.hook_order(HOOK_ARTICLE_BUTTON).is_empty());
    }
}
