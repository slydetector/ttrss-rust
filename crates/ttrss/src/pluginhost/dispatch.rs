//! Plugin method dispatch (M7.3) — the two PHP endpoints that invoke plugin
//! methods by name:
//!
//! - **`PluginHandler` backend op** (protected): `backend.php?op=PluginHandler
//!   &plugin=<n>&method=<m>[&…]` → `PluginHandler::catchall`
//!   (`classes/PluginHandler.php`). The handler-level CSRF gate is a no-op
//!   (`csrf_ignore` is unconditionally `true`); the *plugin-level* gate —
//!   `validate_csrf($token) || $plugin->csrf_ignore($method)` — is enforced
//!   here, exactly like the PHP: unknown plugin / unknown method take
//!   precedence over the CSRF check.
//! - **`pluginhandler` public endpoint** (no session): `public.php?op=
//!   pluginhandler&plugin=<n>&pmethod=<m>` (also reached through the
//!   `?op=<plugin>--<pmethod>` shortcut) → `Handler_Public::pluginhandler`
//!   (`classes/Handler_Public.php`). No CSRF at all — the only gate is the
//!   plugin's `is_public_method`. PHP builds a fresh host loading only the
//!   named plugin (`new PluginHost; load($name, KIND_ALL, 0)`); the port maps
//!   that onto the global registry with a single-plugin active context
//!   ([`HostCtx::single`], owner 0).
//!
//! PHP invokes the method with **no arguments** (`$plugin->$method()`) and
//! the method reads the request superglobals itself; the port hands the
//! request parameter map in as `invoke_method`'s `params` (the M7.1
//! contract: "the request/CLI argument map").

use serde_json::{json, Map, Value};

use std::sync::Arc;

use super::host::{HostCtx, PluginHost};
use sqlx::PgPool;
use super::plugin::{MethodOutput, PluginError};
use crate::session::Sess;
use crate::AppState;

/// PHP's `basename($path)` (no suffix argument).
fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
}

/// `Errors::to_json(code)`.
fn err(code: &str) -> Value {
    json!({ "error": { "code": code, "params": [] } })
}

/// `Errors::to_json(code, $params)` — the port's envelope convention carries
/// named params as a one-element array of objects (see `rpc::unknown_method`).
fn err_with(code: &str, params: Map<String, Value>) -> Value {
    json!({ "error": { "code": code, "params": [ Value::Object(params) ] } })
}

/// The `$_REQUEST`-equivalent for the protected path: the dispatcher's
/// separate fields plus the remaining request params.
fn request_map(op: &str, method: &str, csrf: &str, extra: &Map<String, Value>) -> Value {
    let mut m = Map::new();
    m.insert("op".into(), Value::String(op.into()));
    m.insert("method".into(), Value::String(method.into()));
    m.insert("csrf_token".into(), Value::String(csrf.into()));
    for (k, v) in extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

/// The per-session dispatch context: owner/profile/access from the session,
/// active set = `TTRSS_PLUGINS` (config) + the owner's `_ENABLED_PLUGINS`
/// pref (profile-scoped) — the same lists the asset/hooks paths will use.
/// `Handler_Public`'s login host (PHP's `load_all(KIND_SYSTEM, 0, true)`):
/// every **system** plugin — the `HOOK_AUTH_USER` chain and the
/// `HOOK_POST_LOGOUT` chain run over exactly this set (per-user plugins
/// never participate in auth; the port's `TTRSS_PLUGINS` env var stays
/// meaningful for the non-auth hooks as in PHP).
pub(crate) fn login_ctx(state: &AppState) -> HostCtx {
    HostCtx::system_plugins(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
    )
}

pub async fn session_ctx(state: &AppState, sess: &Sess) -> HostCtx {
    let self_url = sess.self_url();
    let data = sess.data();
    let uid_i64 = data.uid().unwrap_or(0);
    let uid = Some(uid_i64 as i32);
    let profile = data.profile().map(|p| p as i32);
    let access = data.access_level().unwrap_or(0) as i32;
    let system: Vec<String> = state
        .config
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let pref_rows = crate::prefs::load_rows(&state.db, uid_i64, profile).await;
    let enabled =
        crate::prefs::resolve(crate::prefs::_ENABLED_PLUGINS, &pref_rows, profile).as_str().to_string();
    let user: Vec<String> = enabled
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let sys: Vec<&str> = system.iter().map(|s| s.as_str()).collect();
    let usr: Vec<&str> = user.iter().map(|s| s.as_str()).collect();
    HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        uid,
        profile,
        access,
        &sys,
        &usr,
    )
    .with_self_url(self_url)
    .with_cache_dir(state.config.cache_dir.clone())
}

/// The owner-scoped context without a web session (feed output, M7.6):
/// default profile, access 0, user list = the owner's `_ENABLED_PLUGINS`
/// (default profile).
pub async fn owner_ctx(state: &AppState, owner_uid: i32) -> HostCtx {
    let system: Vec<String> = state
        .config
        .plugins
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let pref_rows = crate::prefs::load_rows(&state.db, owner_uid as i64, None).await;
    let enabled =
        crate::prefs::resolve(crate::prefs::_ENABLED_PLUGINS, &pref_rows, None).as_str().to_string();
    let user: Vec<String> = enabled
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let sys: Vec<&str> = system.iter().map(|s| s.as_str()).collect();
    let usr: Vec<&str> = user.iter().map(|s| s.as_str()).collect();
    HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(owner_uid),
        None,
        0,
        &sys,
        &usr,
    )
    .with_self_url(state.config.self_url.clone())
    .with_cache_dir(state.config.cache_dir.clone())
}

/// Build an owner-scoped [`HostCtx`] from the pipeline pieces — no `AppState` /
/// `&Db` needed, the owner's enabled plugins are read straight off the pool.
/// The update pipeline (M7.9) has `pool` + `Config` + the registry but not an
/// `AppState` (which the HTTP layer owns). Mirrors [`owner_ctx`]: system plugins
/// (config) + the owner's `_ENABLED_PLUGINS` (default profile, access 0).
pub(crate) async fn owner_ctx_from(
    host: Arc<PluginHost>,
    pool: PgPool,
    cfg: crate::core::config::Config,
    system: &[String],
    self_url: &str,
    owner_uid: i32,
) -> HostCtx {
    let pref_rows = crate::prefs::load_rows_pool(&pool, owner_uid as i64, None).await;
    let enabled =
        crate::prefs::resolve(crate::prefs::_ENABLED_PLUGINS, &pref_rows, None).as_str().to_string();
    let user: Vec<String> = enabled
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let sys: Vec<&str> = system.iter().map(|s| s.as_str()).collect();
    let usr: Vec<&str> = user.iter().map(|s| s.as_str()).collect();
    HostCtx::for_user(host, pool, cfg, Some(owner_uid), None, 0, &sys, &usr)
        .with_self_url(self_url.to_string())
}

/// `PluginHandler::catchall` — `backend.php?op=PluginHandler&plugin=<n>&method=<m>[&…]`.
///
/// The dispatcher (`crate::rpc::dispatch`) has already validated the session
/// (the op is `Handler_Protected`), lowercased `method`, and passed the
/// handler-level CSRF gate. `method` arrives lowercased, as PHP's
/// `catchall` receives it.
pub async fn pluginhandler(
    state: &AppState,
    sess: &Sess,
    csrf: &str,
    method: &str,
    extra: &Map<String, Value>,
) -> Result<MethodOutput, PluginError> {
    let plugin_name = crate::util::clean_str(extra.get("plugin").and_then(|v| v.as_str()).unwrap_or(""));
    // PHP's per-request host is loaded (the system + owner's active set)
    // before `catchall` runs — a plugin not active for this user is an
    // unknown plugin.
    let mut ctx = session_ctx(state, sess).await;
    let Some((host, idx)) = ctx.get_plugin_detached(&plugin_name) else {
        return Ok(MethodOutput::Json(err("E_UNKNOWN_PLUGIN")));
    };
    let plugin = host.plugin(idx);
    if !plugin.has_public_method(method) {
        return Ok(MethodOutput::Json(err("E_UNKNOWN_METHOD")));
    }
    if !crate::rpc::validate_csrf(sess, csrf) && !plugin.csrf_ignore(method) {
        return Ok(MethodOutput::Json(err("E_UNAUTHORIZED")));
    }
    plugin
        .invoke_method(&mut ctx, method, &request_map("PluginHandler", method, csrf, extra))
        .await
}

/// Run one `run_hooks`-style chain (PHP: `run_hooks($hook, ...)` — void, the
/// hooks *print* into the host's output buffer) and return the drained
/// buffer — the M7.22 prefs-tab call sites. The PHP `ob_start`/trim special
/// case (only `prefFeeds`) is the caller's.
pub(crate) async fn prefs_chain<'a>(
    state: &'a AppState,
    sess: &'a Sess,
    hook: &str,
    id: &str,
) -> String {
    // Clone the captures the future needs (`id`) so the `async move` *owns*
    // them — the `run_hooks` `for<'x>` bound requires the returned future to
    // be valid for any `'x`, which a borrowed `&str` capture can't satisfy.
    let id = id.to_string();
    let mut ctx = session_ctx(state, sess).await;
    ctx
        .run_hooks(hook, |plugin, host| {
            let id = id.clone();
            if hook == crate::pluginhost::HOOK_PREFS_TAB {
                Box::pin(async move { plugin.hook_prefs_tab(host, &id).await })
            } else {
                Box::pin(async move { plugin.hook_prefs_tab_section(host, &id).await })
            }
        })
        .await;
    std::mem::take(&mut ctx.output)
}

/// `Handler_Public::pluginhandler` — the no-account public endpoint.
/// `plugin_param`/`method` are the raw `$_REQUEST["plugin"]` /
/// `$_REQUEST["pmethod"]` values (either the direct `op=pluginhandler` form
/// or the `plugin--pmethod` shortcut); `request` is the full request map the
/// method sees instead of `$_REQUEST`.
pub async fn public_dispatch(
    state: &AppState,
    plugin_param: &str,
    method: &str,
    request: &Value,
    self_url: &str,
) -> Result<MethodOutput, PluginError> {
    // PHP: `basename(clean($_REQUEST["plugin"]))` — unlike the protected
    // path, the name is basename'd (a `plugin` of `../x` cannot escape).
    let name = basename(&crate::util::clean_str(plugin_param));
    let method = crate::util::clean_str(method);
    // PHP dispatch host: `new PluginHost; load($name, KIND_ALL, 0)` — the
    // named plugin must be loadable for the method call. But plugin methods
    // that run chains (e.g. share's public `get` → `HOOK_RENDER_ARTICLE`)
    // target `PluginHost::getInstance()` — the global instance, which on an
    // unauthenticated public request holds the SYSTEM plugins
    // (`init_plugins`, `functions.php:424`). Model that with the system
    // list ∪ the named plugin as the active set.
    let system: Vec<&str> = state
        .config
        .plugins
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut ctx = HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(0),
        None,
        0,
        &system,
        &[name.as_str()],
    )
    .with_self_url(self_url.to_string())
    .with_cache_dir(state.config.cache_dir.clone());
    let Some((host, idx)) = ctx.get_plugin_detached(&name) else {
        let mut params = Map::new();
        params.insert("plugin".into(), Value::String(name));
        return Ok(MethodOutput::Json(err_with("E_UNKNOWN_PLUGIN", params)));
    };
    let plugin = host.plugin(idx);
    if !plugin.has_public_method(&method) {
        return Ok(MethodOutput::Json(err("E_UNKNOWN_METHOD")));
    }
    if !plugin.is_public_method(&method) {
        return Ok(MethodOutput::Json(err("E_UNAUTHORIZED")));
    }
    plugin.invoke_method(&mut ctx, &method, request).await
}
