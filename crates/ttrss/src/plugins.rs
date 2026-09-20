//! Port of the `Pref_Prefs` plugin handlers (`getPluginsList`,
//! `getAvailablePlugins`) in classes/Pref_Prefs.php.
//!
//! `getPluginsList` enumerates the bundled plugins. Each plugin's `about()`
//! metadata (description/author/is_system/more_info) is static in the frozen
//! reference, so it's reproduced here as a data table. `system_enabled` comes
//! from `Config::PLUGINS`, `user_enabled` from the `_ENABLED_PLUGINS` pref,
//! `has_data` from the (empty) plugin config store.

pub mod note;
pub mod nsfw;

use serde_json::{json, Value};

use crate::core::user::ACCESS_LEVEL_ADMIN;
use crate::prefs;
use crate::session::Sess;
use crate::AppState;

/// Static `about()` metadata for the bundled plugins, in the order the
/// `PluginHost` loads them (before the final name-sort). Fields:
/// `(name, is_local, is_system, version, author, description, more_info)`.
const PLUGIN_META: &[(&str, bool, bool, &str, &str, &str, &str)] = &[
    (
        "af_comics",
        false,
        false,
        "",
        "fox",
        "Fixes RSS feeds of assorted comic strips",
        "",
    ),
    (
        "af_psql_trgm",
        false,
        false,
        "",
        "fox",
        "Marks similar articles as read (requires pg_trgm)",
        "",
    ),
    (
        "af_youtube_embed",
        false,
        false,
        "",
        "fox",
        "Embed videos in Youtube RSS feeds (and whitelist Youtube iframes)",
        "",
    ),
    (
        "af_zz_vidmute",
        false,
        false,
        "",
        "fox",
        "Mute audio in HTML5 videos",
        "",
    ),
    (
        "auth_internal",
        false,
        true,
        "",
        "fox",
        "Authenticates against internal tt-rss database",
        "",
    ),
    (
        "auth_remote",
        false,
        true,
        "",
        "fox",
        "Authenticates against external passwords (HTTP Authentication, SSL certificates)",
        "",
    ),
    (
        "auto_assign_labels",
        false,
        false,
        "",
        "fox",
        "Assign labels automatically based on article title, content, and tags",
        "",
    ),
    (
        "bookmarklets",
        false,
        false,
        "",
        "fox",
        "Easy feed subscription and web page sharing using bookmarklets",
        "https://tt-rss.org/docs/Share-Anything.html",
    ),
    (
        "cache_starred_images",
        false,
        false,
        "",
        "fox",
        "Automatically cache media files in Starred articles",
        "",
    ),
    (
        "hotkeys_force_top",
        false,
        false,
        "",
        "itsamenathan",
        "Force open article to the top",
        "",
    ),
    (
        "hotkeys_noscroll",
        false,
        false,
        "",
        "fox",
        "n/p (and up/down) hotkeys move between articles without scrolling",
        "",
    ),
    (
        "hotkeys_swap_jk",
        false,
        false,
        "",
        "fox",
        "Swap j and k hotkeys (for vi brethren)",
        "",
    ),
    (
        "nginx_xaccel",
        true,
        true,
        "v25.12-6c1afee1, by fox",
        "fox",
        "Sends static files via nginx X-Accel-Redirect header",
        "https://github.com/tt-rss/tt-rss-plugin-nginx-xaccel",
    ),
    (
        "no_iframes",
        false,
        false,
        "",
        "fox",
        "Remove embedded iframes (unless whitelisted)",
        "",
    ),
    (
        "note",
        false,
        false,
        "",
        "fox",
        "Adds support for setting article notes",
        "",
    ),
    (
        "nsfw",
        false,
        false,
        "",
        "fox",
        "Hide article content based on tags",
        "",
    ),
    (
        "share",
        false,
        false,
        "",
        "fox",
        "Share article by unique URL",
        "",
    ),
    (
        "shorten_expanded",
        false,
        false,
        "",
        "fox",
        "Shorten overly long articles in CDM/expanded",
        "",
    ),
    (
        "toggle_sidebar",
        false,
        false,
        "",
        "fox",
        "Adds a main toolbar button to toggle sidebar",
        "",
    ),
];

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

    let mut rv: Vec<Value> = Vec::new();
    for (name, is_local, is_system, version, author, description, more_info) in PLUGIN_META {
        let version = crate::util::htmlspecialchars(version);
        rv.push(json!({
            "name": name,
            "is_local": is_local,
            "system_enabled": system_enabled.contains(name),
            "user_enabled": user_enabled.contains(name),
            "has_data": false,
            "is_system": is_system,
            "version": version,
            "author": author,
            "description": description,
            "more_info": more_info,
        }));
    }

    rv.sort_by(|a, b| a["name"].as_str().unwrap().cmp(b["name"].as_str().unwrap()));

    let is_admin = data.access_level().unwrap_or(0) >= ACCESS_LEVEL_ADMIN as i64;

    json!({ "plugins": rv, "is_admin": is_admin })
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
