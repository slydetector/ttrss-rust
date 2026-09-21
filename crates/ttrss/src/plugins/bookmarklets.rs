//! Port of the `bookmarklets` plugin (`tt-rss/plugins/bookmarklets/init.php`).
//!
//! The plugin exposes two bookmarklets on the `prefFeeds` prefs tab:
//! 1. **Subscribe** — a `javascript:` URI that confirms + redirects to the
//!    public `subscribe` method with the current page URL as `feed_url`.
//! 2. **Share** — a `javascript:` URI that opens a popup (`sharepopup`)
//!    pre-filled with the selected text (or page title) + page URL.
//!
//! The `sharepopup` public method (no account required — the login form is
//! rendered when unauthenticated) creates a published article via
//! `article::create_published_article` and closes the window.
//!
//! The `subscribe` public method is **deferred** (it needs the full `Config`
//! for `Feeds::_subscribe` + `rss_utils::update_basic_info`, which the
//! `HostCtx` does not carry); see the M7.14 plan note.

use serde_json::Value;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{
    About, BoxFuture, MethodOutput, Plugin, PluginError,
};
use crate::pluginhost::HOOK_PREFS_TAB;
use crate::{controls, util};

/// PHP's `class Bookmarklets extends Plugin` (stateless → `Copy`).
#[derive(Clone, Copy)]
pub struct Bookmarklets;

impl Plugin for Bookmarklets {
    fn name(&self) -> &'static str {
        "bookmarklets"
    }
    fn class_name(&self) -> &'static str {
        "Bookmarklets"
    }
    fn about(&self) -> About {
        About {
            version: None,
            description: String::from(
                "Easy feed subscription and web page sharing using bookmarklets",
            ),
            author: String::from("fox"),
            system: false,
            more_info: String::from("https://tt-rss.org/docs/Share-Anything.html"),
        }
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_PREFS_TAB);
    }
    fn is_public_method(&self, method: &str) -> bool {
        method.eq_ignore_ascii_case("subscribe") || method.eq_ignore_ascii_case("sharepopup")
    }
    fn has_public_method(&self, method: &str) -> bool {
        matches!(
            method.to_ascii_lowercase().as_str(),
            "subscribe" | "sharepopup"
        )
    }

    /// `Bookmarklets::hook_prefs_tab($tab)` — the `prefFeeds` AccordionPane
    /// with the two bookmarklet links + the "More info…" button.
    fn hook_prefs_tab<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        tab: &'a str,
    ) -> BoxFuture<'a, ()> {
        let this = *self;
        Box::pin(async move {
            if tab != "prefFeeds" {
                return;
            }
            this.hook_prefs_tab_output(host).await;
        })
    }

    /// `Bookmarklets::sharepopup` — the public method dispatch.
    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        method: &'a str,
        params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        let this = *self;
        Box::pin(async move {
            match method.to_ascii_lowercase().as_str() {
                "sharepopup" => this.sharepopup(host, params).await,
                "subscribe" => this.subscribe(host, params).await,
                _ => Err(PluginError::UnknownMethod(method.to_string())),
            }
        })
    }
}

impl Bookmarklets {
    /// `Bookmarklets::hook_prefs_tab` (printed) — the `prefFeeds` AccordionPane.
    async fn hook_prefs_tab_output(&self, host: &mut HostCtx<'_>) {
        let bm_subscribe_url = crate::pluginhost::host::public_method_url(
            &host.self_url,
            self,
            "subscribe",
            &[],
        )
        .unwrap_or_default();
        let bm_share_url = crate::pluginhost::host::public_method_url(
            &host.self_url,
            self,
            "sharepopup",
            &[],
        )
        .unwrap_or_default();

        // PHP: `str_replace("'", "\'", __('Subscribe to %s in Tiny Tiny RSS?'))`
        let confirm_str = "Subscribe to %s in Tiny Tiny RSS?";
        let bm_subscribe_js = format!(
            "javascript:{{if(confirm('{confirm}'.replace('%s',window.location.href)))window.location.href='{url}&feed_url='+encodeURIComponent(window.location.href)}}",
            confirm = confirm_str.replace('\'', "\\'"),
            url = bm_subscribe_url,
        );
        let bm_subscribe_js = util::htmlspecialchars(&bm_subscribe_js);

        let bm_share_js = format!(
            "javascript:(function(){{var d=document,w=window,e=w.getSelection,k=d.getSelection,x=d.selection,s=(e?e():(k)?k():(x?x.createRange().text:0)),f='{url}',l=d.location,e=encodeURIComponent,g=f+'&title='+((e(s))?e(s):e(document.title))+'&url='+e(l.href);function a(){{if(!w.open(g,'t','toolbar=0,resizable=0,scrollbars=1,status=1,width=500,height=250')){{l.href=g;}}}}a();}})()",
            url = bm_share_url,
        );
        let bm_share_js = util::htmlspecialchars(&bm_share_js);

        let more_info_btn = controls::button_tag(
            &format!("{} More info...", controls::icon("help")),
            "",
            &[
                ("class", "alt-info".to_string()),
                (
                    "onclick",
                    "window.open('https://tt-rss.org/docs/Share-Anything.html', '_blank', 'noreferrer')".to_string(),
                ),
            ],
        );

        host.output.push_str(&format!(
            "<div dojoType=\"dijit.layout.AccordionPane\" title=\"<i class='material-icons'>bookmark</i> Bookmarklets\">\n\
             \t<h3>Drag the link below to your browser toolbar, open the feed you're interested in in your browser and click on the link to subscribe to it.</h3>\n\
             \t<label class='dijitButton'>\n\
             \t\t<a href=\"{subscribe_js}\">Subscribe in Tiny Tiny RSS</a>\n\
             \t</label>\n\
             \t<h3>Use this bookmarklet to publish arbitrary pages using Tiny Tiny RSS</h3>\n\
             \t<label class='dijitButton'>\n\
             \t\t<a href=\"{share_js}\">Share with Tiny Tiny RSS</a>\n\
             \t</label>\n\
             \t{more_info}\n\
             </div>\n",
            subscribe_js = bm_subscribe_js,
            share_js = bm_share_js,
            more_info = more_info_btn,
        ));
    }

    /// `Bookmarklets::sharepopup` — the share-this-page popup.
    ///
    /// When `action=share` + the title/url/content/labels are present, creates
    /// a published article + returns a `window.close()` script. Otherwise,
    /// returns the form (or the login form when unauthenticated).
    async fn sharepopup(
        &self,
        host: &mut HostCtx<'_>,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        let uid = host.owner_uid.unwrap_or(0);

        if uid == 0 {
            // Not logged in — the login form (simplified).
            let return_to = crate::pluginhost::host::public_method_url(
                &host.self_url,
                self,
                "sharepopup",
                &[],
            )
            .unwrap_or_default();
            let body = format!(
                "<!DOCTYPE html><html><head><title>Share with Tiny Tiny RSS</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 {}\n\
                 <form action=\"public.php?return={}\" method=\"post\">\n\
                 <input type=\"hidden\" name=\"op\" value=\"login\">\n\
                 <fieldset><label>Login:</label><input name=\"login\" required=\"1\"></fieldset>\n\
                 <fieldset><label>Password:</label><input type=\"password\" name=\"password\" required=\"1\"></fieldset>\n\
                 <fieldset><label> </label><button type=\"submit\" class=\"alt-primary\">Log in</button></fieldset>\n\
                 </form>\n\
                 </div>\n\
                 </body></html>",
                controls::format_error("Not logged in"),
                crate::render::urlencode(&return_to),
            );
            return Ok(MethodOutput::Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body,
            });
        }

        let action = util::clean_str(params.get("action").and_then(|v| v.as_str()).unwrap_or(""));

        if action == "share" {
            let title = util::strip_tags(&util::clean_str(
                params.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            ));
            let url = util::strip_tags(&util::clean_str(
                params.get("url").and_then(|v| v.as_str()).unwrap_or(""),
            ));
            let content = util::strip_tags(&util::clean_str(
                params.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            ));
            let labels = util::strip_tags(&util::clean_str(
                params.get("labels").and_then(|v| v.as_str()).unwrap_or(""),
            ));

            crate::article::create_published_article(
                host.pool,
                &title,
                &url,
                &content,
                &labels,
                uid as i64,
            )
            .await;

            return Ok(MethodOutput::Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: "<!DOCTYPE html><html><head><title>Share with Tiny Tiny RSS</title></head><body><script>window.close();</script></body></html>".to_string(),
            });
        }

        // The form (no action, or action != "share").
        let title = util::htmlspecialchars(&util::clean_str(
            params.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        ));
        let url = util::htmlspecialchars(&util::clean_str(
            params.get("url").and_then(|v| v.as_str()).unwrap_or(""),
        ));
        let csrf = host
            .owner_uid
            .map(|_| String::from(""))
            .unwrap_or_default();

        let form = format!(
            "<!DOCTYPE html><html><head><title>Share with Tiny Tiny RSS</title></head>\n\
             <body class='flat ttrss_utility share_popup css_loading'>\n\
             <div class='content'>\n\
             <form method='post' action='public.php'>\n\
             <input type='hidden' name='op' value='bookmarklets--sharepopup'>\n\
             <input type='hidden' name='csrf_token' value='{csrf}'>\n\
             <input type='hidden' name='action' value='share'>\n\
             <fieldset><label>Title:</label><input style='width : 270px' name='title' value='{title}'></fieldset>\n\
             <fieldset><label>URL:</label><input style='width : 270px' name='url' value='{url}'></fieldset>\n\
             <fieldset><label>Content:</label><input style='width : 270px' name='content' value=''></fieldset>\n\
             <fieldset><label>Labels:</label><input style='width : 270px' name='labels' id='labels_value' placeholder='Alpha, Beta, Gamma' value=''></fieldset>\n\
             <hr/>\n\
             <fieldset>\n\
             {share_btn}\n\
             {cancel_btn}\n\
             <span class='text-muted small'>Shared article will appear in the Published feed.</span>\n\
             </fieldset>\n\
             </form>\n\
             </div>\n\
             </body></html>",
            title = title,
            url = url,
            csrf = csrf,
            share_btn = controls::submit_tag("Share"),
            cancel_btn = controls::button_tag("Cancel", "", &[("onclick", "window.close()".to_string())]),
        );

        Ok(MethodOutput::Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: form,
        })
    }

    /// `Bookmarklets::subscribe` — the subscribe-to-feed page.
    ///
    /// No `feed_url` or no `csrf_token` → the form. With both → calls
    /// `Feeds::_subscribe` and renders the result (warning/notice/error +
    /// the "Edit subscription options" link).
    async fn subscribe(
        &self,
        host: &mut HostCtx<'_>,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        let uid = host.owner_uid.unwrap_or(0);
        let cfg = host.cfg;
        let pool = host.pool;

        let feed_url = util::clean_str(params.get("feed_url").and_then(|v| v.as_str()).unwrap_or(""));
        let csrf = util::clean_str(params.get("csrf_token").and_then(|v| v.as_str()).unwrap_or(""));

        let body = if uid == 0 {
            // Not logged in — the login form (simplified).
            let return_to = crate::pluginhost::host::public_method_url(
                &host.self_url,
                self,
                "subscribe",
                &[],
            )
            .unwrap_or_default();
            format!(
                "<!DOCTYPE html><html><head><title>Subscribe to feed...</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 {}\n\
                 <form action=\"public.php?return={}\" method=\"post\">\n\
                 <input type=\"hidden\" name=\"op\" value=\"login\">\n\
                 <fieldset><label>Login:</label><input name=\"login\" required=\"1\"></fieldset>\n\
                 <fieldset><label>Password:</label><input type=\"password\" name=\"password\" required=\"1\"></fieldset>\n\
                 <fieldset><label> </label><button type=\"submit\" class=\"alt-primary\">Log in</button></fieldset>\n\
                 </form>\n\
                 </div>\n\
                 </body></html>",
                controls::format_error("Not logged in"),
                crate::render::urlencode(&return_to),
            )
        } else if feed_url.is_empty() || csrf.is_empty() {
            // No feed URL or CSRF token — the form.
            format!(
                "<!DOCTYPE html><html><head><title>Subscribe to feed...</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 <h1>Subscribe to feed...</h1>\n\
                 <form method=\"post\" action='public.php'>\n\
                 <input type=\"hidden\" name=\"op\" value=\"bookmarklets--subscribe\">\n\
                 <input type=\"hidden\" name=\"csrf_token\" value=\"{csrf}\">\n\
                 <fieldset><label>Feed or site URL:</label><input style=\"width: 300px\" name=\"feed_url\" value=\"{feed_url_esc}\"></fieldset>\n\
                 <button class=\"alt-primary\" type=\"submit\">Subscribe</button>\n\
                 <a href=\"index.php\">Return to Tiny Tiny RSS</a>\n\
                 </form>\n\
                 </div>\n\
                 </body></html>",
                csrf = csrf,
                feed_url_esc = util::htmlspecialchars(&feed_url),
            )
        } else {
            // Subscribe to the feed.
            let rc = crate::feeds::subscribe(
                cfg,
                pool,
                uid as i64,
                host.access_level,
                &feed_url,
                0,
                "",
                "",
                0,
            )
            .await;

            let code = rc.get("code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

            let mut out = String::new();
            match code {
                0 => out.push_str(&controls::format_warning(&format!(
                    "Already subscribed to <b>{}</b>.",
                    util::htmlspecialchars(&feed_url)
                ))),
                1 => out.push_str(&controls::format_notice(&format!(
                    "Subscribed to <b>{}</b>.",
                    util::htmlspecialchars(&feed_url)
                ))),
                2 => out.push_str(&controls::format_error(&format!(
                    "Could not subscribe to <b>{}</b>.",
                    util::htmlspecialchars(&feed_url)
                ))),
                3 => out.push_str(&controls::format_error(&format!(
                    "No feeds found in <b>{}</b>.",
                    util::htmlspecialchars(&feed_url)
                ))),
                4 => {
                    // Multiple feed URLs found — the select form.
                    let feeds = rc.get("feeds").and_then(|v| v.as_object());
                    if let Some(feeds) = feeds {
                        let mut options = String::new();
                        for (url, name) in feeds {
                            options.push_str(&format!(
                                "<option value=\"{}\">{}</option>",
                                util::htmlspecialchars(url),
                                util::htmlspecialchars(name.as_str().unwrap_or(url)),
                            ));
                        }
                        out.push_str(&format!(
                            "<form method='post' action='public.php'>\n\
                             <input type='hidden' name='op' value='bookmarklets--subscribe'>\n\
                             <input type='hidden' name='csrf_token' value='{csrf}'>\n\
                             <fieldset><label style='display : inline'>Multiple feed URLs found:</label>\
                             <select name='feed_url'>{options}</select></fieldset>\n\
                             <button class='alt-primary' type='submit'>Subscribe to selected feed</button>\n\
                             <a href='index.php'>Return to Tiny Tiny RSS</a>\n\
                             </form>",
                            csrf = csrf,
                            options = options,
                        ));
                    }
                }
                5 => out.push_str(&controls::format_error(&format!(
                    "Could not subscribe to <b>{}</b>.<br>Can't download the Feed URL.",
                    util::htmlspecialchars(&feed_url)
                ))),
                _ => {}
            }

            // The "Edit subscription options" link (code 0 or 1).
            if code <= 2 {
                let feed_id: i32 = sqlx::query_scalar(
                    "SELECT id FROM ttrss_feeds WHERE feed_url = $1 AND owner_uid = $2",
                )
                .bind(&feed_url)
                .bind(uid)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
                .unwrap_or(0);

                if feed_id > 0 {
                    let self_url = &host.self_url;
                    out.push_str(&format!(
                        "<form method='GET' action=\"{self_url}prefs.php\">\n\
                         <input type='hidden' name='tab' value='feeds'>\n\
                         <input type='hidden' name='method' value='editfeed'>\n\
                         <input type='hidden' name='methodparam' value='{feed_id}'>\n\
                         <button class='alt-info' type='submit'>Edit subscription options</button>\n\
                         <a href='index.php'>Return to Tiny Tiny RSS</a>\n\
                         </form>",
                    ));
                }
            }

            if code == 4 {
                // Already handled in the switch above.
            } else if out.is_empty() {
                out.push_str("<a href='index.php'>Return to Tiny Tiny RSS</a>");
            }

            format!(
                "<!DOCTYPE html><html><head><title>Subscribe to feed...</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 {out}\n\
                 </div>\n\
                 </body></html>",
            )
        };

        Ok(MethodOutput::Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body,
        })
    }
}
