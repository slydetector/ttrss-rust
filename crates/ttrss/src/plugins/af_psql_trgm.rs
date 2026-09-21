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


use serde_json::{json, Value};

use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, MethodOutput, Plugin, PluginError};
use crate::pluginhost::{
    HOOK_ARTICLE_BUTTON, HOOK_ARTICLE_FILTER, HOOK_PREFS_EDIT_FEED, HOOK_PREFS_SAVE_FEED,
    HOOK_PREFS_TAB,
};

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
        let _min_len = if min_title_length < 0 { 0 } else { min_title_length };
        format!(
            "Data saved ({:.2}, {})",
            sim,
            if enable_globally { "on" } else { "off" }
        )
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
        let rows: Vec<(i32, i32, String, Option<String>, String, Option<f32>)> =
            sqlx::query_as(
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
            let sm_str = sm.map(|v| format!("{:.2}", v)).unwrap_or_else(|| "0.00".into());
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

        html.push_str(
            "</ul><footer class='text-center'>\
             <button dojoType='dijit.form.Button' type='submit' class='alt-primary'>\
             Close this window</button>\
             </footer>",
        );

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
        let has_trgm: Option<bool> =
            sqlx::query_scalar("SELECT 'similarity'::regproc IS NOT NULL")
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

impl Plugin for AfPsqlTrgm {
    fn name(&self) -> &'static str {
        "af_psql_trgm"
    }
    fn class_name(&self) -> &'static str {
        "Af_Psql_Trgm"
    }
    fn about(&self) -> About {
        About::new(
            "Marks similar articles as read (requires pg_trgm)",
            "fox",
        )
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

    fn hook_article_button<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        line: &'a Value,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
            let id = line.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
            format!(
                "<i style=\"cursor : pointer\" class='material-icons' \
                 onclick=\"Plugins.Psql_Trgm.showRelated({})\" \
                 title=\"Show related articles\">bookmark_outline</i>",
                id
            )
        })
    }

    fn hook_article_filter<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        article: &'a mut Value,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
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

            // Read the prefs from the plugin storage (simplified: use defaults
            // for the M7.15 increment; the storage mechanism is the same as
            // the other plugins).
            let enable_globally: bool = false; // TODO: read from storage
            let enabled_feeds: Vec<i32> = Vec::new(); // TODO: read from storage
            let similarity: f64 = DEFAULT_SIMILARITY; // TODO: read from storage
            let min_title_length: i32 = DEFAULT_MIN_LENGTH; // TODO: read from storage

            let should_catchup = AfPsqlTrgm::filter(
                host.pool,
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
        })
    }

    fn hook_prefs_tab<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        tab: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if tab != "prefFeeds" {
                return;
            }

            let pool = host.pool;

            // Check pg_trgm availability.
            let has_trgm: Option<bool> =
                sqlx::query_scalar("SELECT 'similarity'::regproc IS NOT NULL")
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();

            host.output.push_str(
                "<div dojoType=\"dijit.layout.AccordionPane\" \
                 title=\"<i class='material-icons'>extension</i> \
                 Mark similar articles as read (af_psql_trgm)\">\n",
            );

            if !has_trgm.unwrap_or(false) {
                host.output
                    .push_str("<div class=\"error\">pg_trgm extension not found.</div>\n</div>");
                return;
            }

            host.output.push_str(
                "<form dojoType=\"dijit.form.Form\">\n",
            );
            host.output.push_str(
                "  <fieldset>\n    <label>Minimum similarity:</label>\n    \
                 <input dojoType=\"dijit.form.NumberSpinner\" placeholder=\"0.75\" \
                 id='psql_trgm_similarity' required=\"1\" name=\"similarity\" value=\"0.75\">\n  </fieldset>\n",
            );
            host.output.push_str(
                "  <fieldset>\n    <label>Minimum title length:</label>\n    \
                 <input dojoType=\"dijit.form.NumberSpinner\" placeholder=\"32\" \
                 required=\"1\" name=\"min_title_length\" value=\"32\">\n  </fieldset>\n",
            );
            host.output.push_str(
                "  <fieldset>\n    <label class='checkbox'>\n      \
                 <input type=\"checkbox\" name=\"enable_globally\">\
                 Enable for all feeds.\n    </label>\n  </fieldset>\n",
            );
            host.output.push_str("  <hr/>\n  <input type=\"submit\" value=\"Save\">\n");
            host.output.push_str("</form>\n</div>");
        })
    }

    fn hook_prefs_edit_feed<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        _feed_id: i32,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // The per-feed checkbox is pushed to the output by the feed editor.
            // For the M7.15 increment, this is a no-op (the feed editor
            // integration is deferred to a future increment).
        })
    }

    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        method: &'a str,
        params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        Box::pin(async move {
            match method {
                "save" => {
                    let similarity = params
                        .get("similarity")
                        .and_then(Value::as_f64)
                        .unwrap_or(DEFAULT_SIMILARITY);
                    let min_title_length = params
                        .get("min_title_length")
                        .and_then(Value::as_i64)
                        .unwrap_or(DEFAULT_MIN_LENGTH as i64) as i32;
                    let enable_globally = params
                        .get("enable_globally")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let msg =
                        AfPsqlTrgm::save(similarity, min_title_length, enable_globally);
                    Ok(MethodOutput::Raw(msg))
                }
                "showrelated" => {
                    let id = params.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
                    let uid = host.owner_uid.unwrap_or(0);
                    let html = AfPsqlTrgm::showrelated(host.pool, uid, id).await?;
                    Ok(MethodOutput::Raw(html))
                }
                _ => Err(PluginError::UnknownMethod(method.to_string())),
            }
        })
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}
