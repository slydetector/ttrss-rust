//! The `share` plugin (`tt-rss/plugins/share/init.php`, M7.13) — share an
//! article by a unique URL. The key is a short uuid in the
//! **`ttrss_user_entries.uuid` column** (keyed `ref_id` + `owner_uid`; the
//! PHP's `int_id` is the port's `ref_id`), *not* plugin storage.
//!
//! PHP's `init` registers `HOOK_ARTICLE_BUTTON` (the per-row share icon) +
//! `HOOK_PREFS_TAB_SECTION` (the "Unshare all" section on the published
//! prefs tab). The `PluginHandler` methods are `newkey` (generate/refresh the
//! uuid → `{"link": …}`), `unshare` (clear one), `clearArticleKeys` (clear
//! all of the owner's), `shareDialog` (the read-side dialog), and `get` (the
//! no-account public route serving one article by key — the only public
//! method; `api_version` 2).
//!
//! The frontend (`share.js` / `share_prefs.js`, already in the shell) POSTs
//! `share/newkey`, `share/unshare`, `share/shareDialog`,
//! `share/clearArticleKeys` (raw HTML / JSON replies); the public route
//! `?op=share--get&key=…` serves the article HTML (or a 404).
//!
//! The PHP has no unit tests for this plugin; the port's DB tests are new
//! behavior coverage (key lifecycle, profile scoping, public route).

use serde_json::{json, Value};
use sqlx::Row;

use crate::controls::{button_tag, format_error, format_notice, icon, submit_tag};
use crate::pluginhost::host::{public_method_url, HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, MethodOutput, Plugin, PluginError};
use crate::pluginhost::{HOOK_ARTICLE_BUTTON, HOOK_PREFS_TAB_SECTION};
use crate::util::{clean_str, uniqid_short};

/// PHP's `class Share extends Plugin` (stateless → `Copy`, so the async
/// dispatch can copy it across the `Box::pin` boundary).
#[derive(Clone, Copy)]
pub struct Share;

impl Plugin for Share {
    fn name(&self) -> &'static str {
        "share"
    }
    fn class_name(&self) -> &'static str {
        "Share"
    }
    fn about(&self) -> About {
        About::new("Share article by unique URL", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_ARTICLE_BUTTON);
        host.add_hook_default(HOOK_PREFS_TAB_SECTION);
    }
    fn is_public_method(&self, method: &str) -> bool {
        method.eq_ignore_ascii_case("get")
    }
    fn has_public_method(&self, method: &str) -> bool {
        matches!(
            method.to_ascii_lowercase().as_str(),
            "newkey" | "unshare" | "cleararticlekeys" | "sharedialog" | "get"
        )
    }
    fn get_js(&self) -> String {
        include_str!("../../../../tt-rss/plugins/share/share.js").to_string()
    }
    fn get_css(&self) -> String {
        include_str!("../../../../tt-rss/plugins/share/share.css").to_string()
    }
    fn get_prefs_js(&self) -> String {
        include_str!("../../../../tt-rss/plugins/share/share_prefs.js").to_string()
    }

    fn hook_article_button<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        line: &'a Value,
    ) -> BoxFuture<'a, String> {
        // PHP uses `int_id` (not `id`) for the icon suffix and the onclick
        // argument, and keeps the space before an empty `$icon_class`.
        let int_id = line["int_id"].as_i64().unwrap_or(0);
        let is_shared = line["uuid"].as_str().is_some_and(|u| !u.is_empty());
        Box::pin(async move {
            let icon_class = if is_shared { "is-shared" } else { "" };
            format!(
                "<i class='material-icons icon-share share-icon-{int_id} {icon_class}' \
                 style='cursor : pointer' onclick=\"Plugins.Share.shareArticle({int_id})\" \
                 title=\"Share by URL\">link</i>"
            )
        })
    }

    fn hook_prefs_tab_section<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        section: &'a str,
    ) -> BoxFuture<'a, ()> {
        let this = *self;
        Box::pin(async move {
            if section != "prefFeedsPublishedGenerated" {
                return;
            }
            this.hook_prefs_tab_section_output(host).await;
        })
    }

    /// `Share::newkey` / `unshare` / `clearArticleKeys` / `shareDialog` /
    /// `get` — the dynamic method dispatch.
    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        method: &'a str,
        params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        let this = *self;
        Box::pin(async move {
            match method.to_ascii_lowercase().as_str() {
                "newkey" => Ok(MethodOutput::Json(new_key(host, params).await?)),
                "unshare" => Ok(MethodOutput::Raw(unshare(host, params).await?)),
                "cleararticlekeys" => Ok(MethodOutput::Raw(clear_article_keys(host).await?)),
                "sharedialog" => Ok(MethodOutput::Raw(this.share_dialog(host, params).await?)),
                "get" => get(host, params).await,
                _ => Err(PluginError::UnknownMethod(method.to_string())),
            }
        })
    }
}

impl Share {
    /// `Share::hook_prefs_tab_section` (printed) — the "Unshare all articles"
    /// section on the published prefs tab.
    async fn hook_prefs_tab_section_output(&self, host: &mut HostCtx<'_>) {
        host.output.push_str(&format!(
            "<hr/>\n{}\n \
             <button class='alt-danger' dojoType='dijit.form.Button' \
             onclick=\"return Plugins.Share.clearKeys()\">\n    {}\n    \
             Unshare all articles</button>\n",
            format_notice("You can disable all articles shared by unique URLs here."),
            icon("delete")
        ));
    }

    /// `Share::shareDialog` — the read-side dialog. Looks up the article's
    /// uuid (by `int_id` + `owner_uid`); when the row exists but the uuid is
    /// empty, a fresh one is generated + stored. The footer always prints
    /// (Unshare / Generate-new-URL / Close).
    async fn share_dialog(&self, host: &mut HostCtx<'_>, params: &Value) -> Result<String, PluginError> {
        let id = php_int_cast(&clean_str(
            params.get("id").and_then(|v| v.as_str()).unwrap_or("0"),
        ));
        let owner = host.owner_uid.unwrap_or(0);

        let mut out = String::new();
        let row: Option<String> = sqlx::query("SELECT uuid FROM ttrss_user_entries \
             WHERE int_id = $1 AND owner_uid = $2")
            .bind(id)
            .bind(owner)
            .fetch_optional(host.pool)
            .await
            .map_err(|e| PluginError::Other(e.to_string()))?
            .map(|r| r.try_get::<String, _>(0).unwrap_or_default());

        match row {
            Some(uuid) if !uuid.is_empty() => {
                out.push_str(&dialog_body(&public_url(host, self, &uuid)?));
            }
            Some(_) => {
                // The row exists but the article is not yet shared: mint a key.
                let uuid = uniqid_short();
                sqlx::query("UPDATE ttrss_user_entries SET uuid = $1 \
                     WHERE int_id = $2 AND owner_uid = $3")
                    .bind(&uuid)
                    .bind(id)
                    .bind(owner)
                    .execute(host.pool)
                    .await
                    .map_err(|e| PluginError::Other(e.to_string()))?;
                out.push_str(&dialog_body(&public_url(host, self, &uuid)?));
            }
            None => out.push_str(&format_error("Article not found.")),
        }

        out.push_str(&footer());
        Ok(out)
    }
}

/// The public method URL for the `get` method (PHP
/// `$this->host->get_public_method_url($this, "get", ["key" => $uuid])`).
fn public_url(host: &HostCtx<'_>, share: &Share, uuid: &str) -> Result<String, PluginError> {
    public_method_url(&host.self_url, share, "get", &[("key".into(), uuid.to_string())])
        .ok_or_else(|| PluginError::Other("get is not a public method".to_string()))
}

/// The dialog body (header + the share URL) — PHP's `<header>…<section>…`.
fn dialog_body(url: &str) -> String {
    let href = crate::util::htmlspecialchars(url);
    format!(
        "<header>You can share this article by the following unique URL:</header>\n \
         <section>\n    <div class='panel text-center'>\n        \
         <a class='target-url' href=\"{href}\" target='_blank' rel='noopener \
         noreferrer'>{href}</a>\n    </div>\n</section>\n"
    )
}

/// The dialog footer (always printed): Unshare / Generate-new-URL / Close.
fn footer() -> String {
    format!(
        "<footer class='text-center'>{unshare}{newurl}{close}</footer>",
        unshare = button_tag(
            "Unshare article",
            "",
            &[
                ("class", "alt-danger".to_string()),
                ("onclick", "App.dialogOf(this).unshare()".to_string()),
            ],
        ),
        newurl = button_tag(
            "Generate new URL",
            "",
            &[("onclick", "App.dialogOf(this).newurl()".to_string())],
        ),
        close = submit_tag("Close this window"),
    )
}

/// `Share::newkey` — mint a fresh uuid for the article, echo `{"link": …}`.
async fn new_key(host: &mut HostCtx<'_>, params: &Value) -> Result<Value, PluginError> {
    let id: i32 = clean_str(params.get("id").and_then(|v| v.as_str()).unwrap_or("")).parse().map_err(|_| PluginError::Other("invalid article id".to_string()))?;
    let owner = host.owner_uid.unwrap_or(0);
    let uuid = uniqid_short();

    sqlx::query("UPDATE ttrss_user_entries SET uuid = $1 WHERE int_id = $2 AND owner_uid = $3")
        .bind(&uuid)
        .bind(id)
        .bind(owner)
        .execute(host.pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

    Ok(json!({ "link": uuid }))
}

/// `Share::unshare` — clear the article's uuid, echo "Article unshared".
async fn unshare(host: &mut HostCtx<'_>, params: &Value) -> Result<String, PluginError> {
    let id: i32 = clean_str(params.get("id").and_then(|v| v.as_str()).unwrap_or("")).parse().map_err(|_| PluginError::Other("invalid article id".to_string()))?;
    let owner = host.owner_uid.unwrap_or(0);

    sqlx::query("UPDATE ttrss_user_entries SET uuid = '' WHERE int_id = $1 AND owner_uid = $2")
        .bind(id)
        .bind(owner)
        .execute(host.pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

    Ok("Article unshared".to_string())
}

/// `Share::clearArticleKeys` — clear all of the owner's article uuids.
async fn clear_article_keys(host: &mut HostCtx<'_>) -> Result<String, PluginError> {
    let owner = host.owner_uid.unwrap_or(0);

    sqlx::query("UPDATE ttrss_user_entries SET uuid = '' WHERE owner_uid = $1")
        .bind(owner)
        .execute(host.pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

    Ok("Shared URLs cleared.".to_string())
}

/// `Share::get` — the no-account public route: serve one article by key (or a
/// 404).
async fn get(host: &mut HostCtx<'_>, params: &Value) -> Result<MethodOutput, PluginError> {
    let uuid = clean_str(params.get("key").and_then(|v| v.as_str()).unwrap_or(""));
    if !uuid.is_empty() {
        let row: Option<(i32, i32)> = sqlx::query_as(
            "SELECT ref_id, owner_uid FROM ttrss_user_entries WHERE uuid = $1",
        )
        .bind(&uuid)
        .fetch_optional(host.pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;
        if let Some((ref_id, owner_uid)) = row {
            let body = format_article(host, ref_id, owner_uid).await?;
            return Ok(MethodOutput::Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body,
            });
        }
    }
    Ok(MethodOutput::Response {
        status: 404,
        content_type: "text/plain; charset=utf-8",
        body: String::from("Article not found."),
    })
}

/// `Share::format_article` — the article page (PHP's private `format_article`).
async fn format_article(host: &mut HostCtx<'_>, id: i32, owner_uid: i32) -> Result<String, PluginError> {
    let row: Option<(
        i32, String, String, String, i32, Option<String>, Option<String>, Option<String>, Option<bool>, Option<bool>,
    )> = sqlx::query_as(
        "SELECT id, title, link, content, feed_id, lang,\n\
         (SELECT site_url FROM ttrss_feeds WHERE id = feed_id) as site_url,\n\
         (SELECT title FROM ttrss_feeds WHERE id = feed_id) as feed_title,\n\
         (SELECT hide_images FROM ttrss_feeds WHERE id = feed_id) as hide_images,\n\
         (SELECT always_display_enclosures FROM ttrss_feeds WHERE id = feed_id) as always_display_enclosures\n\
         FROM ttrss_entries, ttrss_user_entries\n\
         WHERE id = $1 AND ref_id = id AND owner_uid = $2",
    )
    .bind(id)
    .bind(owner_uid)
    .fetch_optional(host.pool)
    .await
    .map_err(|e| PluginError::Other(e.to_string()))?;

    let Some((id, title, link, content, _feed_id, lang, site_url, _feed_title, hide_images, _always_display_enclosures)) = row else {
        return Err(PluginError::Other("article not found".to_string()));
    };

    let sanitized = crate::sanitizer::sanitize(
        &content,
        hide_images.unwrap_or(false),
        hide_images.unwrap_or(false),
        true,
        false,
        site_url.as_deref(),
        None,
        id,
        Some(host),
    ).await;

    // M7.22 item 3 — the `HOOK_RENDER_ARTICLE` chain (PHP `share/init.php:149`):
    // the public shared-article page runs the per-article render hooks (nsfw
    // wraps the content in a toggle). The article map is the row the PHP
    // `$line` carries for this route: the selected fields + `tags`.
    let tags = crate::article::get_tags(host.pool, owner_uid as i64, id, Some(owner_uid as i64), None).await;
    let mut article = serde_json::Map::new();
    article.insert("id".into(), serde_json::json!(id));
    article.insert("title".into(), serde_json::json!(title.clone()));
    article.insert("link".into(), serde_json::json!(link.clone()));
    article.insert("lang".into(), serde_json::json!(lang.clone()));
    article.insert("site_url".into(), serde_json::json!(site_url.clone()));
    article.insert(
        "content".into(),
        serde_json::Value::String(sanitized.clone()),
    );
    article.insert(
        "tags".into(),
        serde_json::Value::Array(tags.into_iter().map(serde_json::Value::String).collect()),
    );
    let mut article_val = serde_json::Value::Object(article);
    host
        .chain_hooks_callback(
            crate::pluginhost::HOOK_RENDER_ARTICLE,
            &mut article_val,
            |plugin, host, art| {
                Box::pin(async move {
                    let result = plugin.hook_render_article(host, art).await;
                    // The PHP callback rebinds `$line = $result`.
                    *art = result.clone();
                    result
                })
            },
            |_r, _p| false,
        )
        .await;
    let content_out = article_val
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();

    let enclosures = crate::article::get_enclosures(host.pool, id).await;
    let (og_image, _og_stream, _og_kind) =
        crate::article::get_image(host, &enclosures, &content_out, site_url.as_deref().unwrap_or(""), &article_val).await;
    let og_image = if og_image.is_empty() {
        format!("{}/images/favicon-512px.png", host.self_url)
    } else {
        og_image
    };

    let (author, updated): (String, String) =
        sqlx::query(
            "SELECT COALESCE(author, ''), TO_CHAR(updated AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') FROM ttrss_entries WHERE id = $1",
        )
        .bind(id)
        .fetch_one(host.pool)
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?
        .try_get::<(String, String), _>(0)
        .unwrap_or((String::new(), String::new()));

    let title_esc = crate::util::htmlspecialchars(&title);
    let link_esc = crate::util::htmlspecialchars(&link);
    let og_image_esc = crate::util::htmlspecialchars(&og_image);
    let og_desc = crate::util::truncate_string(
        &crate::util::strip_tags(&title),
        500,
        "...",
    );
    let og_desc_esc = crate::util::htmlspecialchars(&og_desc);

    let header = if !link.is_empty() {
        format!(
            "<h1><a rel='noopener noreferrer' href=\"{}\">{}</a></h1>",
            link_esc, title_esc
        )
    } else {
        format!("<h1>{}</h1>", title_esc)
    };

    Ok(format!(
        "<!DOCTYPE html>\n<html>\n<head>\n\
         <meta http-equiv='Content-Type' content='text/html; charset=utf-8'/>\n\
         <title>{title_esc}</title>\n\
         <meta property='og:title' content=\"{title_esc}\">\n\
         <meta property='og:description' content=\"{og_desc_esc}\">\n\
         <meta property='og:image' content=\"{og_image_esc}\">\n\
         </head>\n\
         <body class='flat ttrss_utility ttrss_zoom css_loading'>\n\
         <div class='container'>\n\
         <div class='content post'>\n\
         <div class='header'>\n\
         <div class='row'>\n{header}\n</div>\n\
         <div class='row'>\n<div>{author}</div>\n<div>{updated}</div>\n</div>\n\
         </div>\n\
         <div class='content' lang=\"{lang}\">{content_out}</div>\n\
         </div>\n</div>\n</body>\n</html>",
        lang = lang.unwrap_or_else(|| String::from("en")),
        author = author,
        updated = updated,
    ))
}

/// PHP's `(int)` cast: the leading numeric run (sign + digits), else 0.
fn php_int_cast(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut end = 0;
    if b.first().is_some_and(|c| *c == b'-' || *c == b'+') {
        end = 1;
    }
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    s.get(..end).and_then(|p| p.parse::<i32>().ok()).unwrap_or(0)
}
