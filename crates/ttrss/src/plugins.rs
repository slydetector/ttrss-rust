//! Port of the `Pref_Prefs` plugin handlers (`getPluginsList`, `setplugins`,
//! `getAvailablePlugins`) in classes/Pref_Prefs.php.
//!
//! `getPluginsList` enumerates the bundled plugins. Each plugin's `about()`
//! metadata (description/author/is_system/more_info) is static in the frozen
//! reference, so it's reproduced here as a data table. `system_enabled` comes
//! from `Config::PLUGINS`, `user_enabled` from the `_ENABLED_PLUGINS` pref,
//! `has_data` from the (empty) plugin config store.

pub mod af_comics;
pub mod af_psql_trgm;
pub mod af_youtube_embed;
pub mod af_zz_vidmute;
pub mod auth_internal;
pub mod auth_remote;
pub mod auto_assign_labels;
pub mod bookmarklets;
pub mod cache_starred_images;
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

/// Static `about()` metadata for the bundled plugins, in the order the
/// `PluginHost` loads them (before the final name-sort). Fields:
/// `(name, class_name, is_local, is_system, version, author, description,
/// more_info)` — `class_name` is the PHP class (the
/// `ttrss_plugin_storage.name` key).
type PluginMeta<'a> = (
    &'a str,
    &'a str,
    bool,
    bool,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
);

const PLUGIN_META: &[PluginMeta<'static>] = &[
    (
        "af_comics",
        "Af_Comics",
        false,
        false,
        "",
        "fox",
        "Fixes RSS feeds of assorted comic strips",
        "",
    ),
    (
        "af_psql_trgm",
        "Af_Psql_Trgm",
        false,
        false,
        "",
        "fox",
        "Marks similar articles as read (requires pg_trgm)",
        "",
    ),
    (
        "af_youtube_embed",
        "Af_Youtube_Embed",
        false,
        false,
        "",
        "fox",
        "Embed videos in Youtube RSS feeds (and whitelist Youtube iframes)",
        "",
    ),
    (
        "af_zz_vidmute",
        "Af_Zz_Vidmute",
        false,
        false,
        "",
        "fox",
        "Mute audio in HTML5 videos",
        "",
    ),
    (
        "auth_internal",
        "Auth_Internal",
        false,
        true,
        "",
        "fox",
        "Authenticates against internal tt-rss database",
        "",
    ),
    (
        "auth_remote",
        "Auth_Remote",
        false,
        true,
        "",
        "fox",
        "Authenticates against external passwords (HTTP Authentication, SSL certificates)",
        "",
    ),
    (
        "auto_assign_labels",
        "Auto_Assign_Labels",
        false,
        false,
        "",
        "fox",
        "Assign labels automatically based on article title, content, and tags",
        "",
    ),
    (
        "bookmarklets",
        "Bookmarklets",
        false,
        false,
        "",
        "fox",
        "Easy feed subscription and web page sharing using bookmarklets",
        "https://tt-rss.org/docs/Share-Anything.html",
    ),
    (
        "cache_starred_images",
        "Cache_Starred_Images",
        false,
        false,
        "",
        "fox",
        "Automatically cache media files in Starred articles",
        "",
    ),
    (
        "hotkeys_force_top",
        "Hotkeys_Force_Top",
        false,
        false,
        "",
        "itsamenathan",
        "Force open article to the top",
        "",
    ),
    (
        "hotkeys_noscroll",
        "Hotkeys_Noscroll",
        false,
        false,
        "",
        "fox",
        "n/p (and up/down) hotkeys move between articles without scrolling",
        "",
    ),
    (
        "hotkeys_swap_jk",
        "Hotkeys_Swap_JK",
        false,
        false,
        "",
        "fox",
        "Swap j and k hotkeys (for vi brethren)",
        "",
    ),
    (
        "nginx_xaccel",
        "Nginx_XAccel",
        true,
        true,
        "v25.12-6c1afee1, by fox",
        "fox",
        "Sends static files via nginx X-Accel-Redirect header",
        "https://github.com/tt-rss/tt-rss-plugin-nginx-xaccel",
    ),
    (
        "no_iframes",
        "No_Iframes",
        false,
        false,
        "",
        "fox",
        "Remove embedded iframes (unless whitelisted)",
        "",
    ),
    (
        "note",
        "Note",
        false,
        false,
        "",
        "fox",
        "Adds support for setting article notes",
        "",
    ),
    (
        "nsfw",
        "NSFW",
        false,
        false,
        "",
        "fox",
        "Hide article content based on tags",
        "",
    ),
    (
        "share",
        "Share",
        false,
        false,
        "",
        "fox",
        "Share article by unique URL",
        "",
    ),
    (
        "shorten_expanded",
        "Shorten_Expanded",
        false,
        false,
        "",
        "fox",
        "Shorten overly long articles in CDM/expanded",
        "",
    ),
    (
        "toggle_sidebar",
        "Toggle_Sidebar",
        false,
        false,
        "",
        "fox",
        "Adds a main toolbar button to toggle sidebar",
        "",
    ),
    (
        "tweak_article_header",
        "Tweak_Article_Header",
        false,
        false,
        "",
        "slydetector",
        "In the article header for special and category feeds, moves the feed title next to the article name.",
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

    // `has_data` — `count(PluginHost::get_all($plugin)) > 0`: the owner's
    // `ttrss_plugin_storage` rows, keyed by the PHP class name.
    let storage_names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM ttrss_plugin_storage WHERE owner_uid = $1")
            .bind(uid)
            .fetch_all(state.db.pool())
            .await
            .unwrap_or_default();

    let mut rv: Vec<Value> = Vec::new();
    for (name, class, is_local, is_system, version, author, description, more_info) in PLUGIN_META {
        let version = crate::util::htmlspecialchars(version);
        rv.push(json!({
            "name": name,
            "is_local": is_local,
            "system_enabled": system_enabled.contains(name),
            "user_enabled": user_enabled.contains(name),
            "has_data": storage_names.iter().any(|n| n == class),
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
