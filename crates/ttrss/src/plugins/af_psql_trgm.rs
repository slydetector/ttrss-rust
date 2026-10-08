//! M7.15 — the **`af_psql_trgm`** plugin (the trigram-similarity filter:
//! marks articles similar to recent ones as read).
//!
//! PHP reference: `tt-rss/plugins/af_psql_trgm/init.php` (354 LOC).
//!
//! The plugin:
//! - **`save`** (PluginHandler): stores 3 prefs — `similarity` (f64, 0–1),
//!   `min_title_length` (i32), `enable_globally` (bool).
//! - **`showrelated`** (PluginHandler): "Related articles" dialog — queries
//!   `SIMILARITY(title, ?)` over 2 weeks, returns the top 10 as HTML.
//! - **`hook_article_filter`**: the core filter — checks `pg_trgm` availability,
//!   the `enable_globally`/`enabled_feeds` gating, the `similarity` threshold,
//!   the `min_title_length` threshold, then runs `MAX(SIMILARITY(...))` over 1 day.
//!   Sets `force_catchup = true` if the max similarity ≥ threshold.
//! - **`hook_prefs_tab("prefFeeds")`**: the AccordionPane UI (form + feed list).
//! - **`hook_prefs_edit_feed`**: the per-feed checkbox.
//! - **`hook_prefs_save_feed`**: saves the per-feed enable/disable.
//! - **`hook_article_button`**: the `bookmark_outline` icon.
//!
//! Requires the `pg_trgm` PostgreSQL extension (available in the Docker image,
//! must be explicitly enabled via `CREATE EXTENSION pg_trgm`).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::i18n::{__, t_sprintf};
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, MethodOutput, Plugin, PluginError};
use crate::pluginhost::{
    HOOK_ARTICLE_BUTTON, HOOK_ARTICLE_FILTER, HOOK_PREFS_EDIT_FEED, HOOK_PREFS_SAVE_FEED,
    HOOK_PREFS_TAB,
};

/// A related-article row from the `showrelated` query:
/// `(entry_id, feed_id, title, link, feed_title, similarity)`.
type RelatedRow = (i32, i32, String, Option<String>, String, Option<f32>);

/// The `af_psql_trgm` plugin.
#[derive(Clone, Copy)]
pub struct AfPsqlTrgm;

/// The default similarity threshold (PHP: `default_similarity = 0.75`).
pub const DEFAULT_SIMILARITY: f64 = 0.75;
/// The default minimum title length (PHP: `default_min_length = 32`).
pub const DEFAULT_MIN_LENGTH: i32 = 32;

impl AfPsqlTrgm {
    /// The PluginHandler `save` method — stores the 3 prefs.
    ///
    /// PHP `save()` (L19): reads `$_POST["similarity"]`, `$_POST["min_title_length"]`,
    /// `$_POST["enable_globally"]`, stores them via `host->set()`.
    ///
    /// Returns the "Data saved (X, Y)" message.
    pub fn save(similarity: f64, min_title_length: i32, enable_globally: bool) -> String {
        let sim = if similarity < 0.0 {
            0.0
        } else {
            (similarity * 100.0).round() / 100.0
        };
        let _min_len = if min_title_length < 0 {
            0
        } else {
            min_title_length
        };
        let sim_str = format!("{:.2}", sim);
        let global_str = if enable_globally { "on" } else { "off" };
        t_sprintf("Data saved (%s, %d)", &[&sim_str, global_str])
    }

    /// The PluginHandler `showrelated` method — the "Related articles" dialog.
    ///
    /// PHP `showrelated()` (L58): looks up the article by id, queries
    /// `SIMILARITY(title, ?)` over 2 weeks, returns the top 10 as HTML.
    pub async fn showrelated(
        pool: &sqlx::PgPool,
        uid: i32,
        id: i32,
    ) -> Result<String, PluginError> {
        // Look up the article title by id.
        let title: Option<String> = sqlx::query_scalar(
            "SELECT e.title FROM ttrss_entries e \
             JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
             WHERE e.id = $1 AND ue.owner_uid = $2",
        )
        .bind(id)
        .bind(uid)
        .fetch_optional(pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

        let Some(title) = title else {
            return Ok(String::new());
        };

        // Query the related articles (top 10 by similarity, over 2 weeks).
        let rows: Vec<RelatedRow> = sqlx::query_as(
            "SELECT e.id, ue.feed_id, e.title, e.link, \
                        COALESCE(f.title, '') AS feed_title, \
                        SIMILARITY(e.title, $1) AS sm \
                 FROM ttrss_entries e \
                 JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
                 LEFT JOIN ttrss_feeds f ON f.id = ue.feed_id \
                 WHERE e.id != $2 AND ue.owner_uid = $3 \
                   AND e.date_entered >= NOW() - INTERVAL '2 weeks' \
                 ORDER BY sm DESC, e.date_entered DESC \
                 LIMIT 10",
        )
        .bind(&title)
        .bind(id)
        .bind(uid)
        .fetch_all(pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

        let mut html = String::new();
        html.push_str(&format!("<p>{}</p>", escape_html(&title)));
        html.push_str("<ul class='panel panel-scrollable'>");

        for (_aid, _feed_id, a_title, a_link, feed_title, sm) in &rows {
            let sm_str = sm
                .map(|v| format!("{:.2}", v))
                .unwrap_or_else(|| "0.00".into());
            let link = a_link.as_deref().unwrap_or("");
            let feed_link = if feed_title.is_empty() {
                String::new()
            } else {
                format!("<a href=\"#\">{}</a>", escape_html(feed_title))
            };
            html.push_str(&format!(
                "<li style='display : flex'><i class='material-icons'>bookmark_outline</i> \
                 <div style='flex-grow : 2'> \
                 <a target=\"_blank\" rel=\"noopener noreferrer\" href=\"{}\">{}</a> \
                 ({}) &mdash; {}</div></li>",
                escape_html(link),
                escape_html(a_title),
                feed_link,
                sm_str,
            ));
        }

        html.push_str(&format!(
            "</ul><footer class='text-center'>\
             <button dojoType='dijit.form.Button' type='submit' class='alt-primary'>\
             {close}</button>\
             </footer>",
            close = __("Close this window"),
        ));

        Ok(html)
    }

    /// The core `hook_article_filter` logic — the trigram similarity check.
    ///
    /// PHP `hook_article_filter($article)` (L250): the full filter pipeline:
    /// 1. Check `pg_trgm` availability
    /// 2. Check `enable_globally` / `enabled_feeds` gating
    /// 3. Check the `similarity` threshold (skip if < 0.01)
    /// 4. Check the `min_title_length` threshold
    /// 5. Run `MAX(SIMILARITY(title, ?))` over 1 day
    /// 6. If max similarity ≥ threshold, set `force_catchup = true`
    ///
    /// Returns `true` if the article should be force-caught-up (marked read).
    #[allow(clippy::too_many_arguments)] // mirrors the PHP `filter` hook signature
    pub async fn filter(
        pool: &sqlx::PgPool,
        title: &str,
        guid_hashed: &str,
        owner_uid: i32,
        enable_globally: bool,
        enabled_feeds: &[i32],
        feed_id: i32,
        similarity: f64,
        min_title_length: i32,
    ) -> bool {
        // 1. Check pg_trgm availability.
        let has_trgm: Option<bool> = sqlx::query_scalar("SELECT 'similarity'::regproc IS NOT NULL")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
        if !has_trgm.unwrap_or(false) {
            return false;
        }

        // 2. Check the gating: enable_globally OR feed_id in enabled_feeds.
        if !enable_globally && !enabled_feeds.contains(&feed_id) {
            return false;
        }

        // 3. Check the similarity threshold (skip if < 0.01).
        if similarity < 0.01 {
            return false;
        }

        // 4. Check the min_title_length threshold.
        if title.len() < min_title_length as usize {
            return false;
        }

        let max_sim: Option<f32> = sqlx::query_scalar(
            "SELECT MAX(SIMILARITY(e.title, $1)) \
             FROM ttrss_entries e \
             JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
             WHERE e.date_entered >= NOW() - INTERVAL '1 day' \
               AND e.guid != $2 AND ue.owner_uid = $3",
        )
        .bind(title)
        .bind(guid_hashed)
        .bind(owner_uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        // 6. If max similarity ≥ threshold, mark as read.
        max_sim.unwrap_or(0.0) as f64 >= similarity
    }
}

#[async_trait]
impl Plugin for AfPsqlTrgm {
    fn name(&self) -> &'static str {
        "af_psql_trgm"
    }
    fn class_name(&self) -> &'static str {
        "Af_Psql_Trgm"
    }
    fn about(&self) -> About {
        About::new("Marks similar articles as read (requires pg_trgm)", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_ARTICLE_FILTER);
        host.add_hook_default(HOOK_PREFS_TAB);
        host.add_hook_default(HOOK_PREFS_EDIT_FEED);
        host.add_hook_default(HOOK_PREFS_SAVE_FEED);
        host.add_hook_default(HOOK_ARTICLE_BUTTON);
    }

    fn get_js(&self) -> String {
        // The init.js is small and stable; embed it.
        r#"/* global Plugins, xhr, __ */

Plugins.Psql_Trgm = {
	showRelated: function (id) {
		const dialog = new dijit.Dialog({
			title: __("Related articles"),
			content: __("Loading, please wait...")
		});

		const tmph = dojo.connect(dialog, "onShow", null, function (/* e */) {
			dojo.disconnect(tmph);

			xhr.post("backend.php", {op: 'PluginHandler', plugin: 'af_psql_trgm', method: 'showrelated', id: id}, (reply) => {
				dialog.attr('content', reply);
			});
		});

		dialog.show();
	}
};"#
        .to_string()
    }

    async fn hook_article_button(&self, _host: &mut HostCtx, line: &Value) -> String {
        let id = line.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
        let show_related = __("Show related articles");
        format!(
            "<i style=\"cursor : pointer\" class='material-icons' \
             onclick=\"Plugins.Psql_Trgm.showRelated({})\" \
             title=\"{show_related}\">bookmark_outline</i>",
            id
        )
    }

    async fn hook_article_filter(&self, host: &mut HostCtx, article: &mut Value) -> Value {
        // Extract the fields from the article.
        let title = article
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let guid_hashed = article
            .get("guid_hashed")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let owner_uid = article
            .get("owner_uid")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        let feed_id = article
            .get("feed")
            .and_then(|f| f.get("id"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;

        // Read the prefs from the plugin storage (PHP: host->get).
        use crate::core::phpser::Value as PValue;
        let enable_globally: bool = host
            .storage_get(&AfPsqlTrgm, "enable_globally", PValue::Bool(false))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Bool(b) => Some(b),
                _ => None,
            })
            .unwrap_or(false);
        let enabled_feeds: Vec<i32> = host
            .storage_get(&AfPsqlTrgm, "enabled_feeds", PValue::Array(vec![]))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Array(arr) => Some(
                    arr.into_iter()
                        .filter_map(|(_, val)| match val {
                            PValue::Int(i) => Some(i as i32),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let similarity: f64 = host
            .storage_get(&AfPsqlTrgm, "similarity", PValue::Float(DEFAULT_SIMILARITY))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Float(f) => Some(f),
                _ => None,
            })
            .unwrap_or(DEFAULT_SIMILARITY);
        let min_title_length: i32 = host
            .storage_get(
                &AfPsqlTrgm,
                "min_title_length",
                PValue::Int(DEFAULT_MIN_LENGTH as i64),
            )
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Int(i) => Some(i as i32),
                _ => None,
            })
            .unwrap_or(DEFAULT_MIN_LENGTH);

        let should_catchup = AfPsqlTrgm::filter(
            host.pool(),
            &title,
            &guid_hashed,
            owner_uid,
            enable_globally,
            &enabled_feeds,
            feed_id,
            similarity,
            min_title_length,
        )
        .await;

        if should_catchup {
            article["force_catchup"] = json!(true);
        }

        article.clone()
    }

    async fn hook_prefs_tab(&self, host: &mut HostCtx, tab: &str) -> () {
        if tab != "prefFeeds" {
            return;
        }

        // Clone the pool (an owned `PgPool`) so it does not borrow `host` —
        // this method mutates `host.output` after querying.
        let pool = host.pool().clone();

        // Check pg_trgm availability.
        let has_trgm: Option<bool> = sqlx::query_scalar("SELECT 'similarity'::regproc IS NOT NULL")
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

        let tab_title = __("Mark similar articles as read (af_psql_trgm)");
        host.output.push_str(&format!(
            "<div dojoType=\"dijit.layout.AccordionPane\" \
             title=\"<i class='material-icons'>extension</i> \
             {tab_title}\">\n",
        ));

        if !has_trgm.unwrap_or(false) {
            host.output
                .push_str("<div class=\"error\">pg_trgm extension not found.</div>\n</div>");
            return;
        }

        use crate::controls;
        use crate::core::phpser::{Key, Value as PValue};

        // The stored prefs (PHP: `$this->host->get(…, default)`).
        let similarity: f64 = host
            .storage_get(&AfPsqlTrgm, "similarity", PValue::Float(DEFAULT_SIMILARITY))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Float(f) => Some(f),
                _ => None,
            })
            .unwrap_or(DEFAULT_SIMILARITY);
        let min_title_length: i32 = host
            .storage_get(
                &AfPsqlTrgm,
                "min_title_length",
                PValue::Int(DEFAULT_MIN_LENGTH as i64),
            )
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Int(i) => Some(i as i32),
                _ => None,
            })
            .unwrap_or(DEFAULT_MIN_LENGTH);
        let enable_globally: bool = host
            .storage_get(&AfPsqlTrgm, "enable_globally", PValue::Bool(false))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Bool(b) => Some(b),
                _ => None,
            })
            .unwrap_or(false);

        let similarity_val = format!("{similarity}");
        let min_len_val = format!("{min_title_length}");
        let similarity_ph = format!("{DEFAULT_SIMILARITY}");
        let min_len_ph = format!("{DEFAULT_MIN_LENGTH}");

        let form_js = "<script type=\"dojo/method\" event=\"onSubmit\" args=\"evt\">\
            \n\t\t\t\t\tevt.preventDefault();\
            \n\t\t\t\t\tif (this.validate()) {\
            \n\t\t\t\t\t\tNotify.progress('Saving data...', true);\
            \n\t\t\t\t\t\txhr.post(\"backend.php\", this.getValues(), (reply) => {\
            \n\t\t\t\t\t\t\tNotify.info(reply);\
            \n\t\t\t\t\t\t})\
            \n\t\t\t\t\t}\
            \n\t\t\t\t</script>";
        let submit = controls::submit_tag(&__("Save"));
        let enable_cb = controls::checkbox_tag("enable_globally", enable_globally, "", &[]);

        host.output
            .push_str("<form dojoType=\"dijit.form.Form\">\n");
        host.output.push_str(&format!(
            "\t{ph}\n",
            ph = controls::pluginhandler_tags("AfPsqlTrgm", "save")
        ));
        host.output.push_str(&format!("\t{js}\n", js = form_js));
        host.output.push_str(&format!(
            "\t{notice}\n",
            notice = controls::format_notice("Enable for specific feeds in the feed editor.")
        ));
        let min_sim_label = __("Minimum similarity:");
        let sim_tooltip = __(
            "PostgreSQL trigram extension returns string similarity as a floating point number (0-1). Setting it too low might produce false positives, zero disables checking.",
        );
        host.output.push_str(&format!(
            "\t<fieldset>\n\t\t<label>{min_sim_label}</label>\n\t\t<input \
             dojoType=\"dijit.form.NumberSpinner\" placeholder=\"{similarity_ph}\" \
             id='psql_trgm_similarity' required=\"1\" name=\"similarity\" value=\"{similarity_val}\">\n\
             \t\t<div dojoType='dijit.Tooltip' connectId='psql_trgm_similarity' position='below'>\
             {sim_tooltip}</div>\n\t</fieldset>\n"
        ));
        let min_len_label = __("Minimum title length:");
        host.output.push_str(&format!(
            "\t<fieldset>\n\t\t<label>{min_len_label}</label>\n\t\t<input \
             dojoType=\"dijit.form.NumberSpinner\" placeholder=\"{min_len_ph}\" \
             required=\"1\" name=\"min_title_length\" value=\"{min_len_val}\">\n\t</fieldset>\n"
        ));
        let enable_all = __("Enable for all feeds.");
        host.output.push_str(&format!(
            "\t<fieldset>\n\t\t<label class='checkbox'>\n\t\t\t{cb}\
             {enable_all}\n\t\t</label>\n\t</fieldset>\n",
            cb = enable_cb
        ));
        host.output.push_str(&format!("\t<hr/>\n\t{submit}\n"));
        host.output.push_str("</form>\n");

        // `/* cleanup */`: drop the enabled feeds that no longer exist
        // for this owner and store the pruned list back.
        let owner_uid = host.owner_uid.unwrap_or(0) as i64;
        let enabled_feeds: Vec<i32> = host
            .storage_get(&AfPsqlTrgm, "enabled_feeds", PValue::Array(vec![]))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Array(arr) => Some(
                    arr.into_iter()
                        .filter_map(|(_, val)| match val {
                            PValue::Int(i) => Some(i as i32),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let mut pruned: Vec<i32> = Vec::new();
        for fid in &enabled_feeds {
            let exists: Option<bool> =
                sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2")
                    .bind(*fid as i64)
                    .bind(owner_uid)
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten();
            if exists.is_some() {
                pruned.push(*fid);
            }
        }
        if pruned != enabled_feeds {
            let arr: Vec<(Key, PValue)> = pruned
                .iter()
                .enumerate()
                .map(|(i, f)| (Key::Int(i as i64), PValue::Int(*f as i64)))
                .collect();
            let _ = host
                .storage_set(&AfPsqlTrgm, "enabled_feeds", PValue::Array(arr))
                .await;
        }

        if !pruned.is_empty() {
            let mut items = String::new();
            for fid in &pruned {
                let title = crate::feeds::feed_title(&pool, *fid as i64, owner_uid).await;
                items.push_str(&format!(
                    "\t\t\t\t<li>\n\t\t\t\t\t<i class='material-icons'>rss_feed</i>\
                     \n\t\t\t\t\t<a href='#'\tonclick=\"CommonDialogs.editFeed({fid})\">\
                     {title}\n\t\t\t\t</a>\n\t\t\t</li>\n"
                ));
            }
            let enabled_header = __("Currently enabled for (click to edit):");
            host.output.push_str(&format!(
                "\t<hr/>\n\t<h3>{enabled_header}</h3>\n\
                 \t<ul class=\"panel panel-scrollable list list-unstyled\">\n{items}\t</ul>\n"
            ));
        }
        host.output.push_str("</div>");
    }

    async fn hook_prefs_edit_feed(&self, host: &mut HostCtx, feed_id: i32) -> () {
        // Read the enabled_feeds from storage (PHP: host->get_array).
        use crate::core::phpser::Value as PValue;
        let enabled_feeds: Vec<i32> = host
            .storage_get(&AfPsqlTrgm, "enabled_feeds", PValue::Array(vec![]))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Array(arr) => Some(
                    arr.into_iter()
                        .filter_map(|(_, val)| match val {
                            PValue::Int(i) => Some(i as i32),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();

        let checked = if enabled_feeds.contains(&feed_id) {
            "checked=\"checked\""
        } else {
            ""
        };
        let sim_header = __("Similarity (af_psql_trgm)");
        let mark_read = __("Mark similar articles as read");
        host.output.push_str(&format!(
            "<header>{sim_header}</header>\n\
             <section><fieldset>\n\
              <label class=\"checkbox\">\n\
               <input type=\"checkbox\" name=\"trgm_similarity_enabled\" {checked}>\n\
               {mark_read}\n\
              </label>\n\
             </fieldset></section>"
        ));
    }

    /// `hook_prefs_save_feed($feed_id)` — reads the `trgm_similarity_enabled`
    /// checkbox from the form data and updates the `enabled_feeds` array in
    /// storage. PHP: `hook_prefs_save_feed` (L240).
    async fn hook_prefs_save_feed(&self, host: &mut HostCtx, feed_id: i32, params: &Value) -> () {
        let enable = params
            .get("trgm_similarity_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Read the current enabled_feeds from storage.
        use crate::core::phpser::Value as PValue;
        let enabled_feeds: Vec<i32> = host
            .storage_get(&AfPsqlTrgm, "enabled_feeds", PValue::Array(vec![]))
            .await
            .ok()
            .and_then(|v| match v {
                PValue::Array(arr) => Some(
                    arr.into_iter()
                        .filter_map(|(_, val)| match val {
                            PValue::Int(i) => Some(i as i32),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();

        // Add or remove the feed_id.
        let mut updated = enabled_feeds;
        if enable {
            if !updated.contains(&feed_id) {
                updated.push(feed_id);
            }
        } else {
            updated.retain(|&id| id != feed_id);
        }

        // Write back to storage.
        let arr: Vec<(crate::core::phpser::Key, PValue)> = updated
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    crate::core::phpser::Key::Int(i as i64),
                    PValue::Int(id as i64),
                )
            })
            .collect();
        let _ = host
            .storage_set(&AfPsqlTrgm, "enabled_feeds", PValue::Array(arr))
            .await;
    }
    fn has_public_method(&self, method: &str) -> bool {
        matches!(method, "save" | "showrelated")
    }
    async fn invoke_method(
        &self,
        host: &mut HostCtx,
        method: &str,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        match method {
            "save" => {
                let similarity = params
                    .get("similarity")
                    .and_then(Value::as_f64)
                    .unwrap_or(DEFAULT_SIMILARITY);
                let min_title_length = params
                    .get("min_title_length")
                    .and_then(Value::as_i64)
                    .unwrap_or(DEFAULT_MIN_LENGTH as i64)
                    as i32;
                let enable_globally = params
                    .get("enable_globally")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // Write the prefs to the plugin storage (PHP: host->set).
                use crate::core::phpser::Value as PValue;
                host.storage_set(&AfPsqlTrgm, "similarity", PValue::Float(similarity))
                    .await
                    .map_err(|e| PluginError::Other(e.to_string()))?;
                host.storage_set(
                    &AfPsqlTrgm,
                    "min_title_length",
                    PValue::Int(min_title_length as i64),
                )
                .await
                .map_err(|e| PluginError::Other(e.to_string()))?;
                host.storage_set(
                    &AfPsqlTrgm,
                    "enable_globally",
                    PValue::Bool(enable_globally),
                )
                .await
                .map_err(|e| PluginError::Other(e.to_string()))?;
                let msg = AfPsqlTrgm::save(similarity, min_title_length, enable_globally);
                Ok(MethodOutput::Raw(msg))
            }
            "showrelated" => {
                let id = params.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
                let uid = host.owner_uid.unwrap_or(0);
                let html = AfPsqlTrgm::showrelated(host.pool(), uid, id).await?;
                Ok(MethodOutput::Raw(html))
            }
            _ => Err(PluginError::UnknownMethod(method.to_string())),
        }
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}
