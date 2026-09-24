//! The `Pref_Prefs::index` page (the settings/preferences tab) and its
//! `index_prefs` pref list. The tab is a Dojo `AccordionContainer` with three
//! panes — "Personal data / Authentication" (lazy-loads `index_auth`),
//! "Preferences" (this module's `index_prefs` form), and "Plugins"
//! (`index_plugins`, whose list loads lazily). The SPA injects the tab via
//! `innerHTML`, so the fidelity bar is structure + visible text (per
//! AGENTS.md), not whitespace.
//!
//! The pref list mirrors `Pref_Prefs::index_prefs_list`: it walks the static
//! section map (`pref_item_map`), pulls each pref's short description + help
//! text (`pref_help`), resolves the current value, and renders the right
//! control per pref (select/checkbox/spinner/textarea/…).

use minijinja::Environment;
use serde_json::{json, Value};

use crate::controls;
use crate::otp;
use crate::prefs;
use crate::session::Sess;
use crate::settings;
use crate::util;
use crate::AppState;
use crate::i18n::{__, t_sprintf};

/// `Pref_Prefs::BLOCK_SEPARATOR` — the sentinel in `pref_item_map` that
/// renders as an `<hr/>` (omitted when a non-default profile is active).
const BLOCK_SEPARATOR: &str = "BLOCK_SEPARATOR";

/// The `pref_item_map`: section title → the ordered pref names (with
/// `BLOCK_SEPARATOR` sentinels), in the PHP declaration order.
fn pref_item_map() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "General",
            vec![
                prefs::USER_LANGUAGE,
                prefs::USER_TIMEZONE,
                BLOCK_SEPARATOR,
                prefs::USER_CSS_THEME,
                BLOCK_SEPARATOR,
                prefs::ENABLE_API_ACCESS,
                "ENABLE_API_ACCESS_BROWSER",
            ],
        ),
        (
            "Feeds",
            vec![
                prefs::DEFAULT_UPDATE_INTERVAL,
                prefs::FRESH_ARTICLE_MAX_AGE,
                "RECENTLY_READ_MAX_AGE",
                prefs::DEFAULT_SEARCH_LANGUAGE,
                BLOCK_SEPARATOR,
                prefs::ENABLE_FEED_CATS,
                BLOCK_SEPARATOR,
                "CONFIRM_FEED_CATCHUP",
                "ON_CATCHUP_SHOW_NEXT_FEED",
                BLOCK_SEPARATOR,
                "HIDE_READ_FEEDS",
                "HIDE_READ_SHOWS_SPECIAL",
            ],
        ),
        (
            "Articles",
            vec![
                prefs::PURGE_OLD_DAYS,
                "PURGE_UNREAD_ARTICLES",
                BLOCK_SEPARATOR,
                "COMBINED_DISPLAY_MODE",
                "CDM_EXPANDED",
                "CDM_ENABLE_GRID",
                BLOCK_SEPARATOR,
                "CDM_AUTO_CATCHUP",
                prefs::VFEED_GROUP_BY_FEED,
                BLOCK_SEPARATOR,
                "SHOW_CONTENT_PREVIEW",
                prefs::STRIP_IMAGES,
            ],
        ),
        (
            "Digest",
            vec![
                "DIGEST_ENABLE",
                "DIGEST_CATCHUP",
                prefs::DIGEST_PREFERRED_TIME,
                prefs::DIGEST_MIN_SCORE,
            ],
        ),
        (
            "Advanced",
            vec![
                prefs::BLACKLISTED_TAGS,
                BLOCK_SEPARATOR,
                prefs::LONG_DATE_FORMAT,
                prefs::SHORT_DATE_FORMAT,
                BLOCK_SEPARATOR,
                "SSL_CERT_SERIAL",
                BLOCK_SEPARATOR,
                "DISABLE_CONDITIONAL_COUNTERS",
                prefs::HEADLINES_NO_DISTINCT,
            ],
        ),
        ("Debugging", vec!["DEBUG_HEADLINE_IDS"]),
    ]
}

/// The `pref_help` table: (pref name, short description, help text or None),
/// in the PHP declaration order (the order `index_prefs_list` resolves them).
fn pref_help() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    vec![
        ("BLACKLISTED_TAGS", "Blacklisted tags", Some("")),
        ("DEFAULT_SEARCH_LANGUAGE", "Default language", Some("Used for full-text search")),
        ("CDM_AUTO_CATCHUP", "Mark read on scroll", Some("Mark articles as read as you scroll past them")),
        ("CDM_EXPANDED", "Always expand articles", None),
        (
            "COMBINED_DISPLAY_MODE",
            "Combined mode",
            Some("Show flat list of articles instead of separate panels"),
        ),
        ("CONFIRM_FEED_CATCHUP", "Confirm marking feeds as read", None),
        ("DEFAULT_UPDATE_INTERVAL", "Default update interval", None),
        ("DIGEST_CATCHUP", "Mark sent articles as read", None),
        (
            "DIGEST_ENABLE",
            "Enable digest",
            Some("Send daily digest of new (and unread) headlines to your email address"),
        ),
        ("DIGEST_PREFERRED_TIME", "Try to send around this time", Some("Time in UTC")),
        ("ENABLE_API_ACCESS", "Enable API", Some("Allows accessing this account through the API")),
        (
            "ENABLE_API_ACCESS_BROWSER",
            "Enable browser-based API access",
            Some("Allows accessing the API from browsers ('Enable API' must also be enabled)"),
        ),
        ("ENABLE_FEED_CATS", "Enable categories", None),
        ("FRESH_ARTICLE_MAX_AGE", "Maximum age of fresh articles", Some("<strong>hours</strong>")),
        ("RECENTLY_READ_MAX_AGE", "Maximum age of recently read articles", Some("<strong>hours</strong>")),
        ("HIDE_READ_FEEDS", "Hide read feeds", None),
        ("HIDE_READ_SHOWS_SPECIAL", "Always show special feeds", Some("While hiding read feeds")),
        (
            "LONG_DATE_FORMAT",
            "Long date format",
            Some("Syntax is identical to PHP <a href='https://www.php.net/manual/function.date.php'>date()</a> function."),
        ),
        ("ON_CATCHUP_SHOW_NEXT_FEED", "Automatically show next feed", Some("After marking one as read")),
        ("PURGE_OLD_DAYS", "Purge articles older than", Some("<strong>days</strong> (0 disables)")),
        ("PURGE_UNREAD_ARTICLES", "Purge unread articles", None),
        ("SHORT_DATE_FORMAT", "Short date format", None),
        ("SHOW_CONTENT_PREVIEW", "Show content preview in headlines", None),
        ("SSL_CERT_SERIAL", "SSL client certificate", None),
        ("STRIP_IMAGES", "Do not embed media", None),
        ("USER_TIMEZONE", "Time zone", None),
        ("VFEED_GROUP_BY_FEED", "Group by feed", Some("Group multiple-feed output by originating feed")),
        (prefs::USER_LANGUAGE, "Language", None),
        (prefs::USER_CSS_THEME, "Theme", None),
        ("HEADLINES_NO_DISTINCT", "Don't enforce DISTINCT headlines", Some("May produce duplicate entries")),
        ("DEBUG_HEADLINE_IDS", "Show article and feed IDs", Some("In the headlines buffer")),
        ("DISABLE_CONDITIONAL_COUNTERS", "Disable conditional counter updates", Some("May increase server load")),
        ("CDM_ENABLE_GRID", "Grid view", Some("On wider screens, if always expanded")),
        ("DIGEST_MIN_SCORE", "Required score", Some("Include articles with this or above score")),
    ]
}

/// `get_translations()` (`include/functions.php`): the language options for
/// the `USER_LANGUAGE` select, in declaration order.
pub fn get_translations() -> Vec<(&'static str, &'static str)> {
    vec![
        ("auto", "Detect automatically"),
        ("ar_SA", "العربيّة (Arabic)"),
        ("be", "Беларуская"),
        ("bg_BG", "Bulgarian"),
        ("da_DA", "Dansk"),
        ("ca_CA", "Català"),
        ("cs_CZ", "Česky"),
        ("en_US", "English"),
        ("el_GR", "Ελληνικά"),
        ("es", "Español (España)"),
        ("de_DE", "Deutsch"),
        ("fa", "Persian (Farsi)"),
        ("fr_FR", "Français"),
        ("gl", "Galego"),
        ("hu_HU", "Magyar (Hungarian)"),
        ("it_IT", "Italiano"),
        ("ja_JP", "日本語 (Japanese)"),
        ("lv_LV", "Latviešu"),
        ("nb_NO", "Norwegian bokmål"),
        ("nl_NL", "Dutch"),
        ("pl_PL", "Polski"),
        ("ru_RU", "Русский"),
        ("pt_BR", "Portuguese/Brazil"),
        ("pt_PT", "Portuguese/Portugal"),
        ("zh_CN", "Simplified Chinese"),
        ("zh_TW", "Traditional Chinese"),
        ("uk_UA", "Українська"),
        ("sv_SE", "Svenska"),
        ("fi_FI", "Suomi"),
        ("ta", "Tamil"),
        ("tr_TR", "Türkçe"),
    ]
}

/// `$update_intervals_nodefault` (`backend.php`): the `DEFAULT_UPDATE_INTERVAL`
/// options (the "Default interval" entry is omitted here — it is the pref
/// default, added separately by the form).
fn update_intervals_nodefault() -> Vec<(&'static str, &'static str)> {
    vec![
        ("-1", "Disable updates"),
        ("15", "15 minutes"),
        ("30", "30 minutes"),
        ("60", "Hourly"),
        ("240", "4 hours"),
        ("720", "12 hours"),
        ("1440", "Daily"),
        ("10080", "Weekly"),
    ]
}

/// The `USER_TIMEZONE` options: the exact PHP
/// `DateTimeZone::listIdentifiers(ALL_WITH_BC)` list (with "Automatic" first).
fn timezone_options() -> Vec<String> {
    crate::pref_timezones::TIMEZONE_IDS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The `USER_CSS_THEME` options: "" → default, then each theme file's basename
/// → its basename without the `.css` extension. Sources: `themes/*.php`,
/// `themes/*.css`, and the local-themes dir `*.css` (a missing dir → empty).
fn theme_options(state: &AppState) -> Vec<(String, String)> {
    // The theme files under `<docroot>/themes` (`*.php` and `*.css`). The PHP
    // also globs the local-themes dir (`Config::LOCAL_THEMES_DIR`, an optional
    // admin feature); a missing/empty dir contributes nothing to the select.
    let mut files: Vec<String> = Vec::new();
    let themes_dir = state.config.docroot.join("themes");
    if let Ok(entries) = std::fs::read_dir(&themes_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".php") || name.ends_with(".css") {
                files.push(name);
            }
        }
    }
    files.sort();
    let mut themes: Vec<(String, String)> = vec![(String::new(), "default".to_string())];
    for file in files {
        let label = file
            .strip_suffix(".css")
            .map(|s| s.to_string())
            .unwrap_or(file.clone());
        themes.push((file, label));
    }
    themes
}

/// The static data sources the control builder needs (resolved once per render).
struct RenderCtx {
    translations: Vec<(String, String)>,
    intervals: Vec<(String, String)>,
    timezones: Vec<String>,
    ts_languages: Vec<String>,
    theme_opts: Vec<(String, String)>,
    force_purge: i32,
    server_time: String,
}

/// Look up a pref's (short description, help text) from `pref_help`.
fn pref_help_lookup(name: &str) -> (&'static str, Option<&'static str>) {
    pref_help()
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, short, help)| (*short, *help))
        .unwrap_or(("", None))
}

/// Render the form control for one pref (mirrors the per-pref `if/else` chain
/// in `index_prefs_list`). Returns the control HTML.
fn build_control(name: &str, value: &str, ty: prefs::Type, ctx: &RenderCtx) -> String {
    match name {
        prefs::USER_LANGUAGE => controls::select_hash(
            name,
            value,
            &ctx.translations,
            &[("style", "width : 220px; margin : 0px".to_string())],
        ),
        prefs::USER_TIMEZONE => controls::select_tag(
            name,
            value,
            &ctx.timezones,
            &[("dojoType", "dijit.form.FilteringSelect".to_string())],
        ),
        prefs::BLACKLISTED_TAGS => format!(
            "<div><textarea dojoType='dijit.form.SimpleTextarea' rows='4'\n\t\t\t\t\t\t\t\tstyle='width: 500px; font-size : 12px;'\n\t\t\t\t\t\t\t\tname='{}'>{}</textarea><br/>\n<div class='help-text-bottom text-muted'>Never apply these tags automatically (comma-separated list).</div>\n</div>",
            name,
            value
        ),
        prefs::USER_CSS_THEME => {
            let select = controls::select_hash(name, value, &ctx.theme_opts, &[]);
            let btn_customize = controls::button_tag(
                &format!("{} Customize", controls::icon("palette")),
                "",
                &[("onclick", "Helpers.Prefs.customizeCSS()".to_string())],
            );
            let btn_more = controls::button_tag(
                &format!("{} More themes...", controls::icon("open_in_new")),
                "",
                &[
                    ("class", "alt-info".to_string()),
                    ("onclick", "window.open(\"https://tt-rss.org/docs/Themes.html\", \"_blank\", \"noreferrer\")".to_string()),
                ],
            );
            format!("{}{}{}", select, btn_customize, btn_more)
        }
        prefs::DEFAULT_UPDATE_INTERVAL => controls::select_hash(name, value, &ctx.intervals, &[]),
        prefs::DEFAULT_SEARCH_LANGUAGE => controls::select_tag(name, value, &ctx.ts_languages, &[]),
        _ if ty == prefs::Type::Bool => {
            let is_disabled = name == "PURGE_UNREAD_ARTICLES" && ctx.force_purge != 0;
            let is_checked = if is_disabled {
                true
            } else {
                value == "true"
            };
            let attrs: Vec<(&str, String)> = if is_disabled {
                vec![("disabled", "1".to_string())]
            } else {
                vec![]
            };
            let mut control = controls::checkbox_tag(name, is_checked, "true", &attrs);
            if name == "DIGEST_ENABLE" {
                control.push_str(&controls::button_tag(
                    &format!("{} Preview", controls::icon("info")),
                    "",
                    &[
                        ("onclick", "Helpers.Digest.preview()".to_string()),
                        ("style", "margin-left : 10px".to_string()),
                    ],
                ));
            }
            control
        }
        n if n == "FRESH_ARTICLE_MAX_AGE"
            || n == "RECENTLY_READ_MAX_AGE"
            || n == prefs::PURGE_OLD_DAYS
            || n == prefs::LONG_DATE_FORMAT
            || n == prefs::SHORT_DATE_FORMAT
            || n == prefs::DIGEST_MIN_SCORE =>
        {
            let is_disabled = n == prefs::PURGE_OLD_DAYS && ctx.force_purge != 0;
            let mut value = value.to_string();
            let attrs: Vec<(&str, String)> = if is_disabled {
                value = ctx.force_purge.to_string();
                vec![
                    ("disabled", "1".to_string()),
                    ("required", "1".to_string()),
                ]
            } else {
                vec![("required", "1".to_string())]
            };
            if ty == prefs::Type::Int {
                controls::number_spinner_tag(n, &value, &attrs, "")
            } else {
                controls::input_tag(n, &value, "text", &attrs, "")
            }
        }
        prefs::SSL_CERT_SERIAL => {
            let input = controls::input_tag(
                name,
                value,
                "text",
                &[("readonly", "1".to_string())],
                "SSL_CERT_SERIAL",
            );
            // `_get_ssl_certificate_id()` is omitted (no serial → Register disabled).
            let btn_register = controls::button_tag(
                &format!("{} Register", controls::icon("security")),
                "",
                &[
                    ("disabled", "1".to_string()),
                    ("onclick", "dijit.byId('SSL_CERT_SERIAL').attr('value', '')".to_string()),
                ],
            );
            let btn_clear = controls::button_tag(
                &format!("{} Clear", controls::icon("clear")),
                "",
                &[
                    ("class", "alt-danger".to_string()),
                    ("onclick", "dijit.byId('SSL_CERT_SERIAL').attr('value', '')".to_string()),
                ],
            );
            let btn_more = controls::button_tag(
                &format!("{} More info...", controls::icon("help")),
                "",
                &[
                    ("class", "alt-info".to_string()),
                    (
                        "onclick",
                        "window.open(\"https://tt-rss.org/docs/SSL-Certificate-Authentication.html\", \"_blank\", \"noreferrer\")".to_string(),
                    ),
                ],
            );
            format!("{}{}{}{}", input, btn_register, btn_clear, btn_more)
        }
        prefs::DIGEST_PREFERRED_TIME => format!(
            "<input dojoType=\"dijit.form.ValidationTextBox\" id=\"{}\" regexp=\"[012]?\\d:\\d\\d\" placeHolder=\"12:00\" name=\"{}\" value=\"{}\">",
            name,
            name,
            util::htmlspecialchars(value)
        ),
        _ => {
            let regexp = if ty == prefs::Type::Int {
                "regexp=\"^\\d*$\" "
            } else {
                ""
            };
            format!(
                "<input dojoType=\"dijit.form.ValidationTextBox\" {} name=\"{}\" value=\"{}\">",
                regexp,
                name,
                util::htmlspecialchars(value)
            )
        }
    }
}

/// The `index_plugins` pane (the Plugins accordion): a form whose list loads
/// lazily (`Helpers.Plugins.reload()`), plus the toolbar and the admin-gated
/// install/update buttons.
fn plugins_pane(_state: &AppState, sess: &Sess) -> String {
    let is_admin = sess.data().access_level().unwrap_or(0) >= 10;
    let enable_selected = __("Enable selected");
    let reload_title = __("Reload");
    let check_updates = __("Check for updates");
    let install_plugin = __("Install plugin");
    let search_btn = __("Search");
    let select_label = __("Select");
    let all_opt = __("All");
    let none_opt = __("None");
    let loading = __("Loading, please wait...");
    let more_info = __("More info");
    let hidden_op = controls::hidden_tag("op", "Pref_Prefs");
    let hidden_setplugins = controls::hidden_tag("method", "setplugins");
    let btn_enable = controls::button_tag(
        &format!("{} {}", controls::icon("check"), enable_selected),
        "",
        &[
            ("class", "alt-primary".to_string()),
            ("onclick", "Helpers.Plugins.enableSelected()".to_string()),
        ],
    );
    let btn_reload = controls::button_tag(
        &controls::icon("refresh"),
        "",
        &[
            ("title", reload_title),
            ("onclick", "Helpers.Plugins.reload()".to_string()),
        ],
    );
    let mut admin_buttons = String::new();
    if is_admin {
        // The config flags (CHECK_FOR_UPDATES / CHECK_FOR_PLUGIN_UPDATES /
        // ENABLE_PLUGIN_INSTALLER) are admin config not yet ported; render the
        // buttons an admin would see (the M7 plugin-install/update surface).
        admin_buttons = format!(
            "<button class='alt-warning' dojoType='dijit.form.Button' onclick=\"Helpers.Plugins.update()\">{} {check_updates}</button>\n<button dojoType='dijit.form.Button' onclick=\"Helpers.Plugins.install()\">{} {install_plugin}</button>",
            controls::icon("update"),
            controls::icon("add")
        );
    }
    format!(
        r#"<form dojoType="dijit.form.Form" id="changePluginsForm">

		{hidden_op}
		{hidden_setplugins}

		<div dojoType="dijit.layout.BorderContainer" gutters="false">
			<div region="top" dojoType='fox.Toolbar'>
				<div class='pull-right'>
					<input name="search" type="search" onkeyup='Helpers.Plugins.search()' dojoType="dijit.form.TextBox">
					<button dojoType='dijit.form.Button' onclick='Helpers.Plugins.search()'>
						{search_btn}
					</button>
				</div>

				<div dojoType='fox.form.DropDownButton'>
					<span>{select_label}</span>
					<div dojoType='dijit.Menu' style='display: none'>
						<div onclick="Lists.select('prefs-plugin-list', true)" dojoType='dijit.MenuItem'>{all_opt}</div>
						<div onclick="Lists.select('prefs-plugin-list', false)" dojoType='dijit.MenuItem'>{none_opt}</div>
					</div>
				</div>
			</div>

			<div dojoType="dijit.layout.ContentPane" region="center" style="overflow-y : auto">
				<script type="dojo/method" event="onShow">
					Helpers.Plugins.reload();
				</script>

				<ul id="prefs-plugin-list" class="prefs-plugin-list list-unstyled">
					<li class='text-center'>{loading}</li>
				</ul>
			</div>
			<div dojoType="dijit.layout.ContentPane" region="bottom">

				<button dojoType='dijit.form.Button' class="alt-info pull-right" onclick='window.open("https://tt-rss.org/docs/Plugins.html", "_blank", "noreferrer")'>
					<i class='material-icons'>help</i>
					{more_info}
				</button>

				{btn_enable}

				{btn_reload}

				{admin_buttons}
			</div>
		</div>
	</form>"#
    )
}

/// `Pref_Prefs::index`: render the settings/preferences tab (the Dojo
/// `AccordionContainer` with the three panes). The Preferences pane inlines
/// `index_prefs` (this module's pref list); the Plugins pane is
/// [`plugins_pane`]; the Personal/Authentication pane loads `index_auth`
/// lazily.
pub async fn index(state: &AppState, sess: &Sess) -> Result<String, sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);

    // Resolved prefs (name -> (value, type)).
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let all = prefs::get_all(&rows, profile);

    // The static / per-request data sources for the control builder.
    let ctx = RenderCtx {
        translations: get_translations()
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        intervals: update_intervals_nodefault()
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        timezones: timezone_options(),
        ts_languages: crate::feeds::get_ts_languages(pool).await,
        theme_opts: theme_options(state),
        force_purge: state.config.force_article_purge,
        server_time: chrono::Local::now().format("%H:%M").to_string(),
    };

    // Walk the section map, rendering each available pref (mirrors
    // `index_prefs_list`).
    let mut sections: Vec<Value> = Vec::new();
    let mut listed_boolean_prefs: Vec<String> = Vec::new();
    for (title, pref_names) in pref_item_map() {
        let mut items: Vec<Value> = Vec::new();
        for name in pref_names {
            if name == BLOCK_SEPARATOR {
                if profile.is_none() {
                    items.push(json!({ "kind": "sep" }));
                }
                continue;
            }
            // Profile-blacklisted prefs are hidden when a non-default profile is
            // active (the `$this->pref_blacklist` is empty).
            if profile.is_some() && prefs::PROFILE_BLACKLIST.contains(&name) {
                continue;
            }
            let Some((_, pref, ty)) = all.iter().find(|(n, _, _)| n == name) else {
                continue;
            };
            let (short_desc, help) = pref_help_lookup(name);
            if short_desc.is_empty() {
                continue;
            }
            let value = pref.as_str().to_string();
            let control_html = build_control(name, &value, *ty, &ctx);
            let help_text = if name == prefs::DIGEST_PREFERRED_TIME {
                Some(format!(
                    "{}. {}",
                    help.map(__).unwrap_or_default(),
                    t_sprintf("Current server time: %s", &[ctx.server_time.as_str()])
                ))
            } else if name == prefs::FRESH_ARTICLE_MAX_AGE
                || name == "RECENTLY_READ_MAX_AGE"
            {
                Some(format!("<strong>{}</strong>", __("hours")))
            } else {
                help.map(__).filter(|s| !s.is_empty())
            };
            if *ty == prefs::Type::Bool {
                listed_boolean_prefs.push(name.to_string());
            }
            items.push(json!({
                "kind": "field",
                "name": name,
                "short_desc": __(short_desc),
                "control": control_html,
                "help_text": help_text,
            }));
        }
        sections.push(json!({ "title": __(title), "items": items }));
    }

    let hidden_op = controls::hidden_tag("op", "Pref_Prefs");
    let hidden_saveconfig = controls::hidden_tag("method", "saveconfig");
    let boolean_prefs_str = util::htmlspecialchars(&listed_boolean_prefs.join(","));
    let hidden_boolean = controls::hidden_tag("boolean_prefs", &boolean_prefs_str);

    // M7.22 item 7 — the `Pref_Prefs::index` plugin chains: the two
    // `HOOK_PREFS_TAB_SECTION` sites (inside the settings form's center
    // pane, and inside its bottom region) + the `HOOK_PREFS_TAB` site
    // after the Plugins pane.
    let prefs_inside = crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB_SECTION,
        "prefPrefsPrefsInside",
    )
    .await;
    let prefs_outside = crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB_SECTION,
        "prefPrefsPrefsOutside",
    )
    .await;
    let prefs_tabs = crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB,
        "prefPrefs",
    )
    .await;

    let rendered = index_env()
        .get_template("index")
        .expect("index template")
        .render(json!({
            "profile_notice": profile.is_some(),
            "profile_notice_text": __("Some preferences are only available in default profile."),
            "sections": sections,
            "hidden_op": hidden_op,
            "hidden_saveconfig": hidden_saveconfig,
            "hidden_boolean": hidden_boolean,
            "plugins_pane": plugins_pane(state, sess),
            "pref_prefs_inside": prefs_inside,
            "pref_prefs_outside": prefs_outside,
            "pref_prefs_tabs": prefs_tabs,
            "pane_auth": __("Personal data / Authentication"),
            "pane_prefs": __("Preferences"),
            "pane_plugins": __("Plugins"),
            "loading": __("Loading, please wait..."),
            "save_config": __("Save configuration"),
            "save_exit": __("Save and exit"),
            "manage_profiles": __("Manage profiles"),
            "reset_defaults": __("Reset to defaults"),
        }))
        .map_err(sqlx::Error::config)?;
    Ok(rendered)
}
/// `Pref_Prefs::index_auth`: the Personal/Authentication tab — a Dojo
/// `TabContainer` with the "Personal data", "Password", "App passwords" and
/// "Authenticator (OTP)" panes. The SPA injects this via `innerHTML`, so the
/// fidelity bar is structure + visible text (per AGENTS.md). The panes are
/// built in Rust (as in [`plugins_pane`]) because of the branchy auth-module
/// and OTP conditionals.
pub async fn index_auth(state: &AppState, sess: &Sess) -> Result<String, sqlx::Error> {
    let pool = state.db.pool();
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let auth_module = data.get_str("auth_module").unwrap_or("auth_internal");
    // Only `auth_internal` is supported out of the gate; it implements
    // `IAuthModule2`, so the Password pane shows the change-password form.
    let auth_internal = auth_module == "auth_internal";
    let login = data.name().unwrap_or("").to_string();

    // `ttrss_users` row (personal-data fields).
    let user: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT full_name, email FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(pool)
            .await?;
    let full_name = user.as_ref().and_then(|r| r.0.clone()).unwrap_or_default();
    let email = user.as_ref().and_then(|r| r.1.clone()).unwrap_or_default();

    // Authenticator (OTP) pane state.
    let otp_enabled = otp::is_otp_enabled(pool, uid).await?;
    let (otp_secret_fmt, otp_qrcode) = if auth_internal && !otp_enabled {
        let secret = otp::get_otp_secret(pool, uid, false)
            .await?
            .unwrap_or_default();
        let otpurl = format!(
            "otpauth://totp/{}?secret={}&issuer={}",
            urlencoding::encode(&login),
            urlencoding::encode(&secret),
            urlencoding::encode("Tiny Tiny RSS")
        );
        (
            Some(otp::format_otp_secret(&secret)),
            otp::otp_qrcode_data_uri(&otpurl),
        )
    } else {
        (None, None)
    };

    let app_passwords = if auth_internal {
        Some(settings::app_password_list(state, sess).await?)
    } else {
        None
    };

    let password_pane = if auth_internal {
        index_auth_password()
    } else {
        controls::format_notice(&format!(
            "Authentication module used for this session (<b>{}</b>) does not provide an ability to set passwords.",
            auth_module
        ))
    };
    let app_passwords_pane = match app_passwords {
        Some(list) => index_auth_app_passwords(&list),
        None => controls::format_warning(
            "App passwords are only available if <b>auth_internal<b> plugin is enabled.",
        ),
    };
    let otp_pane = index_auth_2fa(
        auth_internal,
        otp_enabled,
        otp_secret_fmt.as_deref(),
        otp_qrcode.as_deref(),
    );

    Ok(format!(
        r#"<div dojoType='dijit.layout.TabContainer'>
    <div dojoType='dijit.layout.ContentPane' title="Personal data">
        {personal}
    </div>
    <div dojoType='dijit.layout.ContentPane' title="Password">
        {password}
    </div>
    <div dojoType='dijit.layout.ContentPane' title="App passwords">
        {app_passwords}
    </div>
    <div dojoType='dijit.layout.ContentPane' title="Authenticator (OTP)">
        {otp}
    </div>
</div>"#,
        personal = index_auth_personal(&full_name, &email),
        password = password_pane,
        app_passwords = app_passwords_pane,
        otp = otp_pane,
    ))
}

/// The Dojo `onSubmit` AJAX handlers the PHP embeds in each form
/// (`<script type="dojo/method">`), kept verbatim for front-end fidelity.
const SCRIPT_PERSONAL: &str = r#"<script type="dojo/method" event="onSubmit" args="evt">
    evt.preventDefault();
    if (this.validate()) {
        Notify.progress('Saving data...', true);
        xhr.post("backend.php", this.getValues(), (reply) => {
            Notify.info(reply);
        })
    }
</script>"#;

const SCRIPT_PASSWORD: &str = r#"<script type="dojo/method" event="onSubmit" args="evt">
    evt.preventDefault();
    if (this.validate()) {
        Notify.progress('Saving data...', true);
        xhr.post("backend.php", this.getValues(), (reply) => {
            Notify.close();
            if (reply.indexOf('ERROR: ') == 0) {
                document.getElementById('pwd_change_infobox').innerHTML =
                reply.replace('ERROR: ', '');
            } else {
                document.getElementById('pwd_change_infobox').innerHTML =
                reply.replace('ERROR: ', '');
                const warn = document.getElementById('default_pass_warning');
                if (warn) Element.hide(warn);
            }
            Element.show('pwd_change_infobox');
        })
    }
</script>"#;

const SCRIPT_OTP_DISABLE: &str = r#"<script type="dojo/method" event="onSubmit" args="evt">
    evt.preventDefault();
    if (this.validate()) {
        Notify.progress('Saving data...', true);
        xhr.post("backend.php", this.getValues(), (reply) => {
            Notify.close();
            if (reply.indexOf('ERROR: ') == 0) {
                Notify.error(reply.replace('ERROR: ', ''));
            } else {
                window.location.reload();
            }
        })
    }
</script>"#;

const SCRIPT_OTP_ENABLE: &str = r#"<script type="dojo/method" event="onSubmit" args="evt">
    evt.preventDefault();
    if (this.validate()) {
        Notify.progress('Saving data...', true);
        xhr.post("backend.php", this.getValues(), (reply) => {
            Notify.close();
            if (reply.indexOf('ERROR:') == 0) {
                Notify.error(reply.replace('ERROR:', ''));
            } else {
                window.location.reload();
            }
        })
    }
</script>"#;

/// `Pref_Prefs::index_auth_personal`.
fn index_auth_personal(full_name: &str, email: &str) -> String {
    format!(
        r#"<form dojoType='dijit.form.Form'>
    {op}
    {method}
    {script}
    <fieldset>
        <label>Full name:</label>
        <input dojoType='dijit.form.ValidationTextBox' name='full_name' required='1' value="{fn}">
    </fieldset>
    <fieldset>
        <label>Email:</label>
        <input dojoType='dijit.form.ValidationTextBox' name='email' required='1' value="{em}">
    </fieldset>
    <hr/>
    {save}
</form>"#,
        op = controls::hidden_tag("op", "Pref_Prefs"),
        method = controls::hidden_tag("method", "changePersonalData"),
        script = SCRIPT_PERSONAL,
        fn = util::htmlspecialchars(full_name),
        em = util::htmlspecialchars(email),
        save = controls::button_tag(
            &format!("{} Save", controls::icon("save")),
            "submit",
            &[("class", "alt-primary".into())],
        ),
    )
}

/// `Pref_Prefs::index_auth_password` (the `IAuthModule2` / `auth_internal` branch).
fn index_auth_password() -> String {
    format!(
        r#"<div style='display : none' id='pwd_change_infobox'></div>
<form dojoType='dijit.form.Form'>
    {op}
    {method}
    {script}
    <fieldset>
        <label>Old password:</label>
        <input dojoType='dijit.form.ValidationTextBox' type='password' required='1' name='old_password'>
    </fieldset>
    <fieldset>
        <label>New password:</label>
        <input dojoType='dijit.form.ValidationTextBox' type='password' regexp='^[^<>]+' required='1' name='new_password'>
    </fieldset>
    <fieldset>
        <label>Confirm password:</label>
        <input dojoType='dijit.form.ValidationTextBox' type='password' regexp='^[^<>]+' required='1' name='confirm_password'>
    </fieldset>
    <hr/>
    {btn}
</form>"#,
        op = controls::hidden_tag("op", "Pref_Prefs"),
        method = controls::hidden_tag("method", "changepassword"),
        script = SCRIPT_PASSWORD,
        btn = controls::button_tag(
            &format!("{} Change password", controls::icon("security")),
            "submit",
            &[("class", "alt-primary".into())],
        ),
    )
}

/// `Pref_Prefs::index_auth_app_passwords`.
fn index_auth_app_passwords(app_passwords_html: &str) -> String {
    format!(
        r#"{notice}
<div id='app_passwords_holder'>
    {list}
</div>
<hr>
<button style='float : left' class='alt-primary' dojoType='dijit.form.Button' onclick="Helpers.AppPasswords.generate()">{add}
    Generate password</button>
<button style='float : left' class='alt-danger' dojoType='dijit.form.Button' onclick="Helpers.AppPasswords.removeSelected()">{del}
    Remove selected</button>"#,
        notice = controls::format_notice(
            "Separate passwords used for API clients. Required if you enable OTP."
        ),
        list = app_passwords_html,
        add = controls::icon("add"),
        del = controls::icon("delete"),
    )
}

/// `Pref_Prefs::index_auth_2fa`.
fn index_auth_2fa(
    auth_internal: bool,
    otp_enabled: bool,
    otp_secret_fmt: Option<&str>,
    otp_qrcode: Option<&str>,
) -> String {
    if !auth_internal {
        return controls::format_notice(
            "OTP is only available when using <b>auth_internal</b> authentication module.",
        );
    }
    if otp_enabled {
        return format!(
            r#"{warning}
<form dojoType='dijit.form.Form'>
    {op}
    {method}
    {script}
    <fieldset>
        <label>Your password:</label>
        <input dojoType='dijit.form.ValidationTextBox' type='password' required='1' name='password'>
    </fieldset>
    <hr/>
    {btn}
</form>"#,
            warning = controls::format_warning(
                "One time passwords are currently enabled. Enter your current password below to disable.",
            ),
            op = controls::hidden_tag("op", "Pref_Prefs"),
            method = controls::hidden_tag("method", "otpdisable"),
            script = SCRIPT_OTP_DISABLE,
            btn = controls::button_tag(
                &format!("{} Disable OTP", controls::icon("lock_open")),
                "submit",
                &[("class", "alt-danger".into())],
            ),
        );
    }
    format!(
        r#"<img src='{qrcode}' style='width: 25%'>
{notice}
<form dojoType='dijit.form.Form'>
    {op}
    {method}
    <fieldset>
        <label>OTP secret:</label>
        <code>{secret}</code>
    </fieldset>
    {script}
    <fieldset>
        <label>Your password:</label>
        <input dojoType='dijit.form.ValidationTextBox' type='password' required='1' name='password'>
    </fieldset>
    <fieldset>
        <label>Verification code:</label>
        <input dojoType='dijit.form.ValidationTextBox' autocomplete='off' required='1' name='otp'>
    </fieldset>
    <hr/>
    {btn}
</form>"#,
        qrcode = otp_qrcode.unwrap_or(""),
        notice = controls::format_notice(
            "You will need to generate app passwords for API clients if you enable OTP.",
        ),
        op = controls::hidden_tag("op", "Pref_Prefs"),
        method = controls::hidden_tag("method", "otpenable"),
        script = SCRIPT_OTP_ENABLE,
        secret = otp_secret_fmt.unwrap_or(""),
        btn = controls::button_tag(
            &format!("{} Enable OTP", controls::icon("lock")),
            "submit",
            &[("class", "alt-primary".into())],
        ),
    )
}

fn index_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template("index", include_str!("../templates/prefs_index.html.jinja"))
            .expect("valid prefs_index template");
        env
    })
}
