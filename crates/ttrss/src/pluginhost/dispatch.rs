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

use super::host::HostCtx;
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
async fn session_ctx<'a>(state: &'a AppState, sess: &'a Sess) -> HostCtx<'a> {
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
        &state.plugins,
        state.db.pool(),
        uid,
        profile,
        access,
        &sys,
        &usr,
    )
    .with_self_url(state.config.self_url.clone())
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
    let Some(plugin) = ctx.get_plugin(&plugin_name) else {
        return Ok(MethodOutput::Json(err("E_UNKNOWN_PLUGIN")));
    };
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
) -> Result<MethodOutput, PluginError> {
    // PHP: `basename(clean($_REQUEST["plugin"]))` — unlike the protected
    // path, the name is basename'd (a `plugin` of `../x` cannot escape).
    let name = basename(&crate::util::clean_str(plugin_param));
    let method = crate::util::clean_str(method);
    // PHP: `new PluginHost; load($name, KIND_ALL, 0)` — a fresh host with
    // only the named plugin (if registered at all).
    let mut ctx = HostCtx::single(&state.plugins, state.db.pool(), &name)
        .with_self_url(state.config.self_url.clone());
    let Some(plugin) = ctx.get_plugin(&name) else {
        let mut params = Map::new();
        params.insert("plugin".into(), Value::String(name));
        return Ok(MethodOutput::Json(err_with("E_UNKNOWN_PLUGIN", params)));
    };
    if !plugin.has_public_method(&method) {
        return Ok(MethodOutput::Json(err("E_UNKNOWN_METHOD")));
    }
    if !plugin.is_public_method(&method) {
        return Ok(MethodOutput::Json(err("E_UNAUTHORIZED")));
    }
    plugin.invoke_method(&mut ctx, &method, request).await
}
