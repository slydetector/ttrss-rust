//! The `nsfw` plugin (`tt-rss/plugins/nsfw/init.php`, M7.5) — hide article
//! content based on tags: a per-owner (per-profile via storage scoping)
//! comma-separated tag list in **plugin storage** (`host->get/set($this,
//! "tags")`), matched against the article's tags.
//!
//! Hooks (`init` registers): `HOOK_RENDER_ARTICLE` / `_CDM` / `_API` —
//! `rewrite_contents` wraps a matching article's `content` in
//! `<details class='nsfw'><summary>Not safe for work (click to toggle)</summary>`;
//! `HOOK_ARTICLE_IMAGE` — a matching article gets the `nsfw.png` placeholder
//! (`[self_url + "/plugins/nsfw/nsfw.png", "", "nsfw"]`) instead of its
//! image; `HOOK_PREFS_TAB` — the "NSFW Plugin" pane on the (top) preferences
//! `prefPrefs` tab (printed to the hook output buffer). The `save` method
//! (via `PluginHandler`) normalizes the textarea (PHP
//! `FeedItem_Common::normalize_categories`) and stores it. `get_css` styles
//! `details.nsfw`.
//!
//! Tag matching is PHP's `array_intersect(explode(",", $stored),
//! $article["tags"]) > 0` — `explode` without trimming, so a stored list of
//! `"a, b"` (the `implode(", ")` form `save` writes) compares `" b"` against
//! the article tags. Ported as-is (the reference relies on the article tags
//! arriving space-trimmed; `save`'s normalization makes round-trips match).

use serde_json::{json, Map, Value};

use crate::controls::{pluginhandler_tags, submit_tag};
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, MethodOutput, Plugin, PluginError};
use crate::pluginhost::{
    HOOK_ARTICLE_IMAGE, HOOK_PREFS_TAB, HOOK_RENDER_ARTICLE, HOOK_RENDER_ARTICLE_API,
    HOOK_RENDER_ARTICLE_CDM,
};
use crate::core::phpser::Value as PValue;
use crate::util::normalize_categories;

/// PHP's `class NSFW extends Plugin`.
#[derive(Clone, Copy)]
pub struct NsFw;

impl Plugin for NsFw {
    fn name(&self) -> &'static str {
        "nsfw"
    }
    fn class_name(&self) -> &'static str {
        "NSFW"
    }
    fn about(&self) -> About {
        About::new("Hide article content based on tags", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_RENDER_ARTICLE);
        host.add_hook_default(HOOK_RENDER_ARTICLE_CDM);
        host.add_hook_default(HOOK_RENDER_ARTICLE_API);
        host.add_hook_default(HOOK_ARTICLE_IMAGE);
        host.add_hook_default(HOOK_PREFS_TAB);
    }
    fn get_css(&self) -> String {
        // PHP's literal (tab-indented, as in the reference).
        "details.nsfw {\n\t\t\tcursor : pointer;\n\t\t\tuser-select : none;\n\t\t}"
            .to_string()
    }

    /// PHP's `method_exists`/method dispatch is case-insensitive; the RPC
    /// layer lowercases `method` before dispatch.
    fn has_public_method(&self, method: &str) -> bool {
        method.eq_ignore_ascii_case("save")
    }

    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        method: &'a str,
        params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        let this = *self;
        Box::pin(async move {
            match method.to_ascii_lowercase().as_str() {
                "save" => {
                    let tags =
                        normalize_categories(
                            &params
                                .get("tags")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .split(',')
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>(),
                        )
                        .join(", ");
                    host.storage_set(&this, "tags", PValue::Bytes(tags.into_bytes()))
                        .await
                        .map_err(|e| PluginError::Other(e.to_string()))?;
                    // PHP: `echo __("Configuration saved.");`
                    Ok(MethodOutput::Raw("Configuration saved.".to_string()))
                }
                _ => Err(PluginError::UnknownMethod(method.to_string())),
            }
        })
    }

    // -- HOOK_RENDER_ARTICLE / _CDM / _API -------------------------------

    fn hook_render_article<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        article: &'a mut Value,
    ) -> BoxFuture<'a, Value> {
        let this = *self;
        Box::pin(async move {
            match rewrite_contents(host, &this, article).await {
                Ok(v) => v,
                // The PHP runner's per-invocation try/catch: log, continue
                // with the article unchanged.
                Err(e) => {
                    tracing::error!(error = %e, "nsfw: rewrite_contents failed");
                    article.clone()
                }
            }
        })
    }

    fn hook_render_article_cdm<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        article: &'a mut Value,
    ) -> BoxFuture<'a, Value> {
        let this = *self;
        Box::pin(async move {
            match rewrite_contents(host, &this, article).await {
                Ok(v) => v,
                // The PHP runner's per-invocation try/catch: log, continue
                // with the article unchanged.
                Err(e) => {
                    tracing::error!(error = %e, "nsfw: rewrite_contents failed");
                    article.clone()
                }
            }
        })
    }

    /// PHP's `hook_render_article_api($row)`: `$article = $row['headline'] ??
    /// $row['article']` — the *article* (not the row) is rewritten and
    /// returned, as in the reference.
    fn hook_render_article_api<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        params: &'a Value,
    ) -> BoxFuture<'a, Value> {
        let this = *self;
        Box::pin(async move {
            let fallback = Value::Object(Map::new());
            let article = params
                .get("headline")
                .or_else(|| params.get("article"))
                .unwrap_or(&fallback);
            match rewrite_contents(host, &this, article).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "nsfw: rewrite_contents failed");
                    article.clone()
                }
            }
        })
    }

    // -- HOOK_ARTICLE_IMAGE ------------------------------------------------

    fn hook_article_image<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        _enclosures: &'a Value,
        content: &'a str,
        _site_url: &'a str,
        article: &'a Value,
    ) -> BoxFuture<'a, Value> {
        let this = *self;
        Box::pin(async move {
            if matches_tags(host, &this, article).await.unwrap_or(false) {
                Value::Array(vec![
                    json!(format!("{}{}", host.self_url, "/plugins/nsfw/nsfw.png")),
                    json!(""),
                    json!("nsfw"),
                ])
            } else {
                Value::Array(vec![json!(""), json!(""), json!(content)])
            }
        })
    }

    // -- HOOK_PREFS_TAB ------------------------------------------------------

    fn hook_prefs_tab<'a, 'h: 'a>(&'a self, host: &'a mut HostCtx<'h>, tab: &'a str) -> BoxFuture<'a, ()> {
        let this = *self;
        Box::pin(async move {
            if tab != "prefPrefs" {
                return;
            }
            let tags = stored_tags(host, &this).await.unwrap_or_default();
            host.output.push_str(&format!(
                "<div dojoType=\"dijit.layout.AccordionPane\" \
                 title=\"<i class='material-icons'>extension</i> NSFW Plugin\">\n \
                 <form dojoType=\"dijit.form.Form\">\n \
                 {tags}\n \
                 <script type=\"dojo/method\" event=\"onSubmit\" args=\"evt\">\n \
                 evt.preventDefault();\n \
                 if (this.validate()) {{\n \
                 Notify.progress('Saving data...', true);\n \
                 xhr.post(\"backend.php\", this.getValues(), (reply) => {{\n \
                 Notify.info(reply);\n \
                 }})\n \
                 }}\n \
                 </script>\n \
                 <header>Tags to consider NSFW (comma-separated):</header>\n \
                 <fieldset>\n \
                 <textarea dojoType='dijit.form.SimpleTextarea' rows='4' \
                 style='width: 500px; font-size : 12px;' \
                 name='tags'>{tags_escaped}</textarea>\n \
                 </fieldset>\n \
                 <hr/>\n \
                 {save}\n \
                 </form>\n \
                 </div>",
                tags = pluginhandler_tags("NSFW", "save"),
                tags_escaped = escape_html(&tags),
                save = submit_tag("Save"),
            ));
        })
    }
}

// -- hook bodies -------------------------------------------------------------

/// PHP's `NSFW::rewrite_contents($article)` — wrap `content` when the stored
/// tag list intersects the article's tags.
async fn rewrite_contents(
    host: &mut HostCtx<'_>,
    this: &NsFw,
    article: &Value,
) -> Result<Value, PluginError> {
    let mut out = article.clone();
    if matches_tags(host, this, article).await? {
        let content = article.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out["content"] = Value::String(format!(
            "<details class='nsfw'><summary>Not safe for work (click to toggle)</summary>{content}</details>"
        ));
    }
    Ok(out)
}

/// PHP's `$tags = explode(",", $this->host->get($this, "tags"))` followed by
/// `count(array_intersect($tags, $article["tags"])) > 0`.
async fn matches_tags(
    host: &mut HostCtx<'_>,
    this: &NsFw,
    article: &Value,
) -> Result<bool, PluginError> {
    let tags = stored_tags(host, this).await?;
    let article_tags: Vec<&str> = article
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str()).collect())
        .unwrap_or_default();
    Ok(tags
        .split(',')
        .any(|t| article_tags.contains(&t)))
}

/// The stored tag list (PHP `get($this, "tags")` — unset reads as `false`,
/// which `explode` coerces to `""`).
async fn stored_tags(host: &mut HostCtx<'_>, this: &NsFw) -> Result<String, PluginError> {
    let v = host
        .storage_get(this, "tags", PValue::Null)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;
    Ok(v.as_str().unwrap_or("").to_string())
}

/// Minimal `htmlspecialchars` for the prefs-pane textarea (PHP's `<?= $tags ?>`
/// is an unescaped echo — a reference flaw; the textarea value needs quote
/// escaping to be valid HTML, so the port escapes: the prefs pane is the
/// port's only rendering of this value).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "&#039;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pluginhost::PluginHost;

    #[test]
    fn css_is_the_php_literal() {
        let p = NsFw;
        assert_eq!(
            p.get_css(),
            "details.nsfw {\n\t\t\tcursor : pointer;\n\t\t\tuser-select : none;\n\t\t}"
        );
    }

    #[test]
    fn method_surface() {
        let p = NsFw;
        for m in ["save", "SAVE", "Save"] {
            assert!(p.has_public_method(m), "{m}");
        }
        for m in ["clear", "tags", "get"] {
            assert!(!p.has_public_method(m), "{m}");
        }
        assert_eq!(p.name(), "nsfw");
        assert_eq!(p.class_name(), "NSFW");
        assert_eq!(p.about().description, "Hide article content based on tags");
        assert_eq!(p.about().author, "fox");
        assert!(!p.about().system);
        assert_eq!(p.api_version(), 2);
    }

    /// The five `init` registrations (PHP's `add_hook` order — priority 50,
    /// so insertion order is the invocation order). Checked through the
    /// runner's `hook_order` (active-set restricted): `nsfw` alone answers
    /// each of the five hooks.
    #[tokio::test]
    async fn init_registers_the_five_hooks() {
        let host = PluginHost::startup(&crate::pluginhost::all_plugins());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let ctx = HostCtx::for_user(&host, &pool, Some(1), None, 0, &[], &["nsfw"]);
        for h in [
            HOOK_RENDER_ARTICLE,
            HOOK_RENDER_ARTICLE_CDM,
            HOOK_RENDER_ARTICLE_API,
            HOOK_ARTICLE_IMAGE,
            HOOK_PREFS_TAB,
        ] {
            let order = ctx.hook_order(h);
            assert_eq!(order.len(), 1, "{h}");
            assert_eq!(host.plugin(order[0]).name(), "nsfw", "{h}");
        }
    }
}
