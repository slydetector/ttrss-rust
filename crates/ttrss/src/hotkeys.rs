//! Hotkey tables + the keyboard-shortcuts help dialog (`RPC::get_hotkeys_info`
//! / `get_hotkeys_map` / `hotkeyHelp`). The tables are PHP declaration-ordered;
//! `hotkeys_map()` feeds the init-params `hotkeys` value, `hotkey_help()`
//! renders the help panel.

use crate::i18n::__;
use crate::util::htmlspecialchars as html_escape;
use serde_json::{Map, Value};

/// `RPC::get_hotkeys_map()` entries — `sequence => action` in PHP declaration
/// order (drives both the init-params `hotkeys` map and the help-dialog list).
const HOTKEYS: &[(&str, &str)] = &[
    ("k", "next_feed"),
    ("K", "next_unread_feed"),
    ("j", "prev_feed"),
    ("J", "prev_unread_feed"),
    ("n", "next_article_noscroll"),
    ("p", "prev_article_noscroll"),
    ("N", "article_page_down"),
    ("P", "article_page_up"),
    ("*(33)|Shift+PgUp", "article_page_up"),
    ("*(34)|Shift+PgDn", "article_page_down"),
    ("{3}(38)|Up", "prev_article_or_scroll"),
    ("{3}(40)|Down", "next_article_or_scroll"),
    ("*(38)|Shift+Up", "article_scroll_up"),
    ("*(40)|Shift+Down", "article_scroll_down"),
    ("^(38)|Ctrl+Up", "prev_article_noscroll"),
    ("^(40)|Ctrl+Down", "next_article_noscroll"),
    ("/", "search_dialog"),
    ("\\", "cancel_search"),
    ("s", "toggle_mark"),
    ("S", "toggle_publ"),
    ("u", "toggle_unread"),
    ("T", "edit_tags"),
    ("o", "open_in_new_window"),
    ("c p", "catchup_below"),
    ("c n", "catchup_above"),
    ("a W", "toggle_widescreen"),
    ("a e", "toggle_full_text"),
    ("e", "email_article"),
    ("a q", "close_article"),
    ("a s", "article_span_grid"),
    ("a a", "select_all"),
    ("a u", "select_unread"),
    ("a U", "select_marked"),
    ("a p", "select_published"),
    ("a i", "select_invert"),
    ("a n", "select_none"),
    ("f r", "feed_refresh"),
    ("f a", "feed_unhide_read"),
    ("f s", "feed_subscribe"),
    ("f e", "feed_edit"),
    ("f q", "feed_catchup"),
    ("f x", "feed_reverse"),
    ("f g", "feed_toggle_vgroup"),
    ("f G", "feed_toggle_grid"),
    ("f D", "feed_debug_update"),
    ("f %", "feed_debug_viewfeed"),
    ("f C", "toggle_combined_mode"),
    ("f c", "toggle_cdm_expanded"),
    ("Q", "catchup_all"),
    ("x", "cat_toggle_collapse"),
    ("g a", "goto_all"),
    ("g f", "goto_fresh"),
    ("g s", "goto_marked"),
    ("g p", "goto_published"),
    ("g r", "goto_read"),
    ("g P", "goto_prefs"),
    ("r", "select_article_cursor"),
    ("c l", "create_label"),
    ("c f", "create_filter"),
    ("c s", "collapse_sidebar"),
    ("?", "help_dialog"),
];

/// `RPC::get_hotkeys_map` — `[prefixes, {sequence: action}]`.
///
/// M7.22 item 2: the `HOOK_HOTKEY_MAP` chain (PHP `RPC.php:629`) runs over
/// the base map **before** the prefixes are computed, so plugins can add or
/// redefine keys (hotkeys_swap_jk, hotkeys_noscroll).
pub async fn hotkeys_map(
    host: &mut crate::pluginhost::host::HostCtx,
) -> (Vec<String>, Map<String, Value>) {
    let mut map: Map<String, Value> = Map::new();
    for (seq, action) in HOTKEYS {
        map.insert((*seq).to_string(), Value::from((*action).to_string()));
    }
    let mut map_val = Value::Object(map);
    host.chain_hooks_callback(
        crate::pluginhost::HOOK_HOTKEY_MAP,
        &mut map_val,
        |plugin, host, hk| {
            Box::pin(async move {
                let result = plugin.hook_hotkey_map(host, hk).await;
                // The PHP callback rebinds `$hotkeys = $result`.
                *hk = result.clone();
                result
            })
        },
        |_, _| false,
    )
    .await;
    let map = map_val.as_object().cloned().unwrap_or_default();

    // Prefixes: the base sequences in table order (a plugin redefining a base
    // key keeps its slot), then any plugin-added sequences.
    let mut prefixes: Vec<String> = Vec::new();
    let add_prefix = |seq: &str, prefixes: &mut Vec<String>| {
        if let Some(first) = seq.split(' ').next()
            && seq.contains(' ')
            && !prefixes.iter().any(|p| p == first)
        {
            prefixes.push(first.to_string());
        }
    };
    for (seq, _) in HOTKEYS {
        if map.contains_key(*seq) {
            add_prefix(seq, &mut prefixes);
        }
    }
    for seq in map.keys() {
        if !HOTKEYS.iter().any(|(k, _)| k == seq) {
            add_prefix(seq, &mut prefixes);
        }
    }
    (prefixes, map)
}

/// PHP `RPC::get_hotkeys_info()` — section → (action → description).
/// `__()` is the identity with no catalog loaded.
const HOTKEY_INFO: &[(&str, &[(&str, &str)])] = &[
    (
        "Navigation",
        &[
            ("next_feed", "Open next feed"),
            ("next_unread_feed", "Open next unread feed"),
            ("prev_feed", "Open previous feed"),
            ("prev_unread_feed", "Open previous unread feed"),
            (
                "next_article_or_scroll",
                "Open next article (in combined mode, scroll down)",
            ),
            (
                "prev_article_or_scroll",
                "Open previous article (in combined mode, scroll up)",
            ),
            ("next_headlines_page", "Scroll headlines by one page down"),
            ("prev_headlines_page", "Scroll headlines by one page up"),
            ("next_article_noscroll", "Open next article"),
            ("prev_article_noscroll", "Open previous article"),
            (
                "next_article_noexpand",
                "Move to next article (don't expand)",
            ),
            (
                "prev_article_noexpand",
                "Move to previous article (don't expand)",
            ),
            ("search_dialog", "Show search dialog"),
            ("cancel_search", "Cancel active search"),
        ],
    ),
    (
        "Article",
        &[
            ("toggle_mark", "Toggle starred"),
            ("toggle_publ", "Toggle published"),
            ("toggle_unread", "Toggle unread"),
            ("edit_tags", "Edit tags"),
            ("open_in_new_window", "Open in new window"),
            ("catchup_below", "Mark below as read"),
            ("catchup_above", "Mark above as read"),
            ("article_scroll_down", "Scroll down"),
            ("article_scroll_up", "Scroll up"),
            ("article_page_down", "Scroll down page"),
            ("article_page_up", "Scroll up page"),
            ("select_article_cursor", "Select article under cursor"),
            ("email_article", "Email article"),
            ("close_article", "Close/collapse article"),
            ("toggle_expand", "Toggle article expansion (combined mode)"),
            ("toggle_widescreen", "Toggle widescreen mode"),
            (
                "toggle_full_text",
                "Toggle full article text via Readability",
            ),
        ],
    ),
    (
        "Article selection",
        &[
            ("select_all", "Select all articles"),
            ("select_unread", "Select unread"),
            ("select_marked", "Select starred"),
            ("select_published", "Select published"),
            ("select_invert", "Invert selection"),
            ("select_none", "Deselect everything"),
        ],
    ),
    (
        "Feed",
        &[
            ("feed_refresh", "Refresh current feed"),
            ("feed_unhide_read", "Un/hide read feeds"),
            ("feed_subscribe", "Subscribe to feed"),
            ("feed_edit", "Edit feed"),
            ("feed_catchup", "Mark as read"),
            ("feed_reverse", "Reverse headlines"),
            ("feed_toggle_vgroup", "Toggle headline grouping"),
            ("feed_toggle_grid", "Toggle grid view"),
            ("feed_debug_update", "Debug feed update"),
            ("feed_debug_viewfeed", "Debug viewfeed()"),
            ("catchup_all", "Mark all feeds as read"),
            ("cat_toggle_collapse", "Un/collapse current category"),
            ("toggle_cdm_expanded", "Toggle auto expand in combined mode"),
            ("toggle_combined_mode", "Toggle combined mode"),
        ],
    ),
    (
        "Go to",
        &[
            ("goto_all", "All articles"),
            ("goto_fresh", "Fresh"),
            ("goto_marked", "Starred"),
            ("goto_published", "Published"),
            ("goto_read", "Recently read"),
            ("goto_prefs", "Preferences"),
        ],
    ),
    (
        "Other",
        &[
            ("create_label", "Create label"),
            ("create_filter", "Create filter"),
            ("collapse_sidebar", "Un/collapse sidebar"),
            ("help_dialog", "Keyboard shortcuts help"),
        ],
    ),
];

/// The help-dialog display form of a hotkey sequence: everything after the
/// first `|` (the PHP `|display` suffix); else multi-char keys expand `*` →
/// `"Shift+"` and `^` → `"Ctrl+"` per character.
fn hotkey_display(seq: &str) -> String {
    if let Some(i) = seq.find('|') {
        return seq[i + 1..].to_string();
    }
    seq.split(' ')
        .map(|k| {
            if k.len() > 1 {
                k.chars()
                    .map(|c| match c {
                        '*' => __("Shift") + "+",
                        '^' => __("Ctrl") + "+",
                        c => c.to_string(),
                    })
                    .collect()
            } else {
                k.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// PHP `RPC::hotkeyHelp()` — the keyboard-shortcuts help dialog (raw HTML).
///
/// `get_hotkeys_info()` + `HOOK_HOTKEY_INFO` (RPC.php:478-549): the chain
/// receives the section → (action → description) map (values `__()`-ed at
/// build time) and its result replaces it. The rendered dialog iterates the
/// (replaced) map — PHP preserves array insertion order; the port's map is
/// key-sorted (visible only if a plugin adds new sections).
pub async fn hotkey_help(host: &mut crate::pluginhost::host::HostCtx) -> String {
    // `$omap`: action → [sequence...] in map declaration order.
    let mut omap: Vec<(&str, Vec<String>)> = Vec::new();
    for (seq, action) in HOTKEYS.iter().copied() {
        let disp = hotkey_display(seq);
        match omap.iter_mut().find(|e| e.0 == action) {
            Some(e) => e.1.push(disp),
            None => omap.push((action, vec![disp])),
        }
    }

    let mut info: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for &(section, actions) in HOTKEY_INFO {
        let mut a = serde_json::Map::new();
        for &(action, desc) in actions {
            a.insert(action.to_string(), Value::from(__(desc)));
        }
        info.insert(__(section), Value::Object(a));
    }
    let mut info_val = Value::Object(info);
    if !host
        .hook_order(crate::pluginhost::HOOK_HOTKEY_INFO)
        .is_empty()
    {
        host.chain_hooks_callback(
            crate::pluginhost::HOOK_HOTKEY_INFO,
            &mut info_val,
            |plugin, host, st| {
                Box::pin(async move {
                    let r = plugin.hook_hotkey_info(host, st).await;
                    *st = r.clone();
                    r
                })
            },
            |_r, _p| false,
        )
        .await;
    }

    let mut out = String::new();
    out.push_str("<ul class='panel panel-scrollable hotkeys-help' style='height : 300px'>");
    for (section, actions) in info_val.as_object().unwrap().iter() {
        out.push_str(&format!(
            "\n<li><h3>{}</h3></li>",
            html_escape(&__(section))
        ));
        if let Some(actions) = actions.as_object() {
            for (action, desc) in actions {
                if let Some((_, seqs)) = omap.iter().find(|e| e.0 == action) {
                    for seq in seqs {
                        out.push_str(&format!(
                            "\n<li>\n<div class='hk'><code>{}</code></div>\n<div class='desc'>{}</div>\n</li>",
                            html_escape(seq),
                            html_escape(desc.as_str().unwrap_or(""))
                        ));
                    }
                }
            }
        }
    }
    let close = __("Close this window");
    out.push_str(&format!(
        "\n</ul>\n<footer class='text-center'>\n<button dojoType=\"dijit.form.Button\" class=\"alt-primary\" type=\"submit\">{close}</button></footer>"
    ));
    out
}

#[cfg(test)]
mod tests {
    use crate::pluginhost::host::HostCtx;
    use serde_json::json;
    use std::sync::Arc;

    /// M7.22 item 2: the HOOK_HOTKEY_MAP chain seam — the base map +
    /// hotkeys_swap_jk / hotkeys_noscroll, and the prefix order (base-table
    /// order preserved).
    #[tokio::test]
    async fn hotkeys_map_runs_hook_chain() {
        let host = Arc::new(crate::pluginhost::PluginHost::startup(
            &crate::pluginhost::all_plugins(),
        ));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();

        // No plugins: base map, prefix order from the table (c, a, f, g).
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &[],
        );
        let (prefixes, map) = crate::hotkeys::hotkeys_map(&mut ctx).await;
        assert_eq!(prefixes, vec!["c", "a", "f", "g"]);
        assert_eq!(map["s"], json!("toggle_mark"));
        // The base already maps j/k (the pre-vi assignment); swap_jk flips it.
        assert_eq!(map["j"], json!("prev_feed"));
        assert_eq!(map["k"], json!("next_feed"));

        // hotkeys_swap_jk: j/k (re)defined to the vi pairs.
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["hotkeys_swap_jk"],
        );
        let (prefixes, map) = crate::hotkeys::hotkeys_map(&mut ctx).await;
        assert_eq!(map["j"], json!("next_feed"));
        assert_eq!(map["J"], json!("next_unread_feed"));
        assert_eq!(map["k"], json!("prev_feed"));
        assert_eq!(map["K"], json!("prev_unread_feed"));
        assert_eq!(prefixes, vec!["c", "a", "f", "g"], "prefix order unchanged");

        // hotkeys_noscroll: arrows + n/p remapped to the noscroll actions.
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["hotkeys_noscroll"],
        );
        let (prefixes, map) = crate::hotkeys::hotkeys_map(&mut ctx).await;
        assert_eq!(map["(40)|Down"], json!("next_article_noscroll"));
        assert_eq!(map["n"], json!("next_article_noscroll"));
        assert_eq!(map["p"], json!("prev_article_noscroll"));
        assert_eq!(prefixes, vec!["c", "a", "f", "g"]);
    }
}
