//! Port of the `Pref_Prefs` plugin handlers (`getPluginsList`, `setplugins`,
//! `getAvailablePlugins`) in classes/Pref_Prefs.php.
//!
//! `getPluginsList` enumerates the bundled plugins: each plugin's `about()`
//! metadata is read from the plugin instance (the `PluginHost` registry
//! holds every bundled plugin, mirroring PHP's `load_all(KIND_ALL)` +
//! `about()` loop). `system_enabled` comes from `Config::PLUGINS`,
//! `user_enabled` from the `_ENABLED_PLUGINS` pref, `has_data` from the
//! plugin config store.

pub mod af_comics;
pub mod af_psql_trgm;
pub mod af_youtube_embed;
pub mod af_zz_vidmute;
pub mod auth_internal;
pub mod auth_remote;
pub mod auto_assign_labels;
pub mod bookmarklets;
pub mod cache_starred_images;
pub mod freshapi;
pub mod hotkeys_force_top;
pub mod hotkeys_noscroll;
pub mod hotkeys_swap_jk;
pub mod no_iframes;
pub mod note;
pub mod nsfw;
pub mod share;
pub mod shorten_expanded;
pub mod toggle_sidebar;
pub mod tweak_article_header;

use serde_json::{Map, Value, json};

use crate::AppState;
use crate::core::user::ACCESS_LEVEL_ADMIN;
use crate::prefs;
use crate::session::Sess;

/// The `tt-rss.org/tt-rss/plugins.json` payload (the plugin-installer catalog).
/// Fetched over the network by the PHP; reproduced verbatim here since the
/// harness has no outbound network. Its `last_update` stamps are normalized by
/// the diff harness.
const AVAILABLE_PLUGINS: &str = include_str!("plugins/available_plugins.json");

/// `Pref_Prefs::getPluginsList`.
pub async fn get_plugins_list(state: &AppState, sess: &Sess) -> Value {
    let system_enabled: Vec<&str> = state
        .config
        .plugins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile();
    let profile_i32 = profile.map(|p| p as i32);
    let pref_rows = prefs::load_rows(&state.db, uid, profile_i32).await;
    let enabled_pref = prefs::resolve(prefs::_ENABLED_PLUGINS, &pref_rows, profile_i32);
    let user_enabled: Vec<&str> = enabled_pref
        .as_str()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    // `has_data` — `count(PluginHost::get_all($plugin)) > 0`: the owner's
    // `ttrss_plugin_storage` rows, keyed by the PHP class name.
    let storage_names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM ttrss_plugin_storage WHERE owner_uid = $1")
            .bind(uid)
            .fetch_all(state.db.pool())
            .await
            .unwrap_or_default();

    // The `PluginHost` registry holds every bundled plugin (PHP builds a
    // throwaway host with `load_all(KIND_ALL)` for this list).
    let mut rv: Vec<Value> = Vec::new();
    for plugin in state.plugins.plugins() {
        let about = plugin.about();
        // `Pref_Prefs::_get_plugin_version`: `Some(v)` → `v%.2f, by <author>`;
        // bundled plugins have no git repo of their own, so `None` → `""`.
        let version = match &about.version {
            Some(v) => format!(
                "v{:.2}, by {}",
                v.parse::<f64>().unwrap_or(0.0),
                about.author
            ),
            None => String::new(),
        };
        rv.push(json!({
            "name": plugin.name(),
            // All bundled; the port has no local-plugins dir.
            "is_local": false,
            "system_enabled": system_enabled.contains(&plugin.name()),
            "user_enabled": user_enabled.contains(&plugin.name()),
            "has_data": storage_names.iter().any(|n| n == plugin.class_name()),
            "is_system": about.system,
            "version": crate::util::htmlspecialchars(&version),
            "author": about.author,
            "description": about.description,
            "more_info": about.more_info,
        }));
    }

    rv.sort_by(|a, b| a["name"].as_str().unwrap().cmp(b["name"].as_str().unwrap()));

    let is_admin = data.access_level().unwrap_or(0) >= ACCESS_LEVEL_ADMIN as i64;

    json!({ "plugins": rv, "is_admin": is_admin })
}

/// `Pref_Prefs::setplugins`: persist the user's enabled-plugin set.
///
/// PHP (`classes/Pref_Prefs.php`):
/// ```php
/// $plugins = array_filter($_REQUEST['plugins'] ?? [], clean(...));
/// Prefs::set(Prefs::_ENABLED_PLUGINS, implode(',', $plugins), $uid, $profile);
/// ```
/// The SPA's "Enable selected" button POSTs the checked `plugins[]` values;
/// `rpc_form_from_urlencoded` normalizes them to `extra["plugins"]` as a string
/// array (PHP `$_REQUEST['plugins']`). Each value is trim/strip-tagged, empties
/// are dropped, and the rest is comma-joined into the `_ENABLED_PLUGINS` pref.
pub async fn set_plugins(state: &AppState, sess: &Sess, extra: &Map<String, Value>) {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile: Option<i32> = data.profile().map(|p| p as i32);

    // `$_REQUEST['plugins']` is an array (see `rpc_form_from_urlencoded`, which
    // turns `name[]` fields into the bare key as an array); tolerate a lone
    // scalar too.
    let mut chosen: Vec<&str> = Vec::new();
    match extra.get("plugins") {
        Some(Value::Array(vals)) => {
            for v in vals {
                if let Some(s) = v.as_str() {
                    chosen.push(s);
                }
            }
        }
        Some(Value::String(s)) => chosen.push(s.as_str()),
        _ => {}
    }

    // PHP `array_filter($_REQUEST['plugins'] ?? [], clean(...))`: trim +
    // strip_tags each element, drop empties, then comma-join.
    let value = chosen
        .iter()
        .map(|s| crate::util::strip_tags(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",");

    let _ = prefs::set(
        &state.db,
        prefs::_ENABLED_PLUGINS,
        &value,
        uid,
        profile,
        true,
    )
    .await;

    // Mirror PHP `Prefs::set`'s in-memory cache update (see settings save-config).
    crate::prefs::invalidate(&state.prefs_cache, uid, profile);
}

/// `Pref_Prefs::getAvailablePlugins`: the installed-plugin catalog for
/// admins (from `Config::ENABLE_PLUGIN_INSTALLER`), else `[]`.
pub fn get_available_plugins(sess: &Sess) -> Value {
    let is_admin = sess.data().access_level().unwrap_or(0) >= ACCESS_LEVEL_ADMIN as i64;
    if is_admin {
        serde_json::from_str(AVAILABLE_PLUGINS).unwrap_or(Value::Array(vec![]))
    } else {
        Value::Array(vec![])
    }
}
