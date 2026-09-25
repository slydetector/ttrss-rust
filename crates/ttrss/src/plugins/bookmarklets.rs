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
//! The `subscribe` public method (the full `Feeds::subscribe` engine: all
//! result codes 0–5, the "Edit subscription options" link, the
//! multiple-feed-URLs select form) was un-deferred at M7.14 when `HostCtx`
//! started carrying `&Config`.

use serde_json::Value;
use async_trait::async_trait;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, MethodOutput, Plugin, PluginError, };
use crate::pluginhost::HOOK_PREFS_TAB;
use crate::{controls, util};
use crate::i18n::{__};

/// PHP's `class Bookmarklets extends Plugin` (stateless → `Copy`).
#[derive(Clone, Copy)]
pub struct Bookmarklets;

#[async_trait]
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
    async fn hook_prefs_tab(
        &self,
        host: &mut HostCtx<'_>,
        tab: &str,
    ) -> () {
        let this = *self;
        if tab != "prefFeeds" {
            return;
        }
        this.hook_prefs_tab_output(host).await;
    }

    /// `Bookmarklets::sharepopup` — the public method dispatch.
    async fn invoke_method(
        &self,
        host: &mut HostCtx<'_>,
        method: &str,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        let this = *self;
        match method.to_ascii_lowercase().as_str() {
            "sharepopup" => this.sharepopup(host, params).await,
            "subscribe" => this.subscribe(host, params).await,
            _ => Err(PluginError::UnknownMethod(method.to_string())),
        }
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
        let confirm_str = __("Subscribe to %s in Tiny Tiny RSS?");
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
            &format!("{} {}", controls::icon("help"), __("More info...")),
            "",
            &[
                ("class", "alt-info".to_string()),
                (
                    "onclick",
                    "window.open('https://tt-rss.org/docs/Share-Anything.html', '_blank', 'noreferrer')".to_string(),
                ),
            ],
        );

        let bl_title = __("Bookmarklets");
        let bl_drag = __("Drag the link below to your browser toolbar, open the feed you're interested in in your browser and click on the link to subscribe to it.");
        let bl_sub = __("Subscribe in Tiny Tiny RSS");
        let bl_use = __("Use this bookmarklet to publish arbitrary pages using Tiny Tiny RSS");
        let bl_share = __("Share with Tiny Tiny RSS");
        host.output.push_str(&format!(
            "<div dojoType=\"dijit.layout.AccordionPane\" title=\"<i class='material-icons'>bookmark</i> {bl_title}\">\n\
             \t<h3>{bl_drag}</h3>\n\
             \t<label class='dijitButton'>\n\
             \t\t<a href=\"{subscribe_js}\">{bl_sub}</a>\n\
             \t</label>\n\
             \t<h3>{bl_use}</h3>\n\
             \t<label class='dijitButton'>\n\
             \t\t<a href=\"{share_js}\">{bl_share}</a>\n\
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
        let share_popup_title = __("Share with Tiny Tiny RSS");
        let login_label = __("Login:");
        let password_label = __("Password:");
        let title_label = __("Title:");
        let url_label = __("URL:");
        let content_label = __("Content:");
        let labels_label = __("Labels:");
        let published_note = __("Shared article will appear in the Published feed.");

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
                "<!DOCTYPE html><html><head><title>{share_popup_title}</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 {}\n\
                 <form action=\"public.php?return={}\" method=\"post\">\n\
                 <input type=\"hidden\" name=\"op\" value=\"login\">\n\
                 <fieldset><label>{login_label}</label><input name=\"login\" required=\"1\"></fieldset>\n\
                 <fieldset><label>{password_label}</label><input type=\"password\" name=\"password\" required=\"1\"></fieldset>\n\
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
                body: format!(
                    "<!DOCTYPE html><html><head><title>{share_popup_title}</title></head><body><script>window.close();</script></body></html>",
                ),
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
            "<!DOCTYPE html><html><head><title>{share_popup_title}</title></head>\n\
             <body class='flat ttrss_utility share_popup css_loading'>\n\
             <div class='content'>\n\
             <form method='post' action='public.php'>\n\
             <input type='hidden' name='op' value='bookmarklets--sharepopup'>\n\
             <input type='hidden' name='csrf_token' value='{csrf}'>\n\
             <input type='hidden' name='action' value='share'>\n\
             <fieldset><label>{title_label}</label><input style='width : 270px' name='title' value='{title}'></fieldset>\n\
             <fieldset><label>{url_label}</label><input style='width : 270px' name='url' value='{url}'></fieldset>\n\
             <fieldset><label>{content_label}</label><input style='width : 270px' name='content' value=''></fieldset>\n\
             <fieldset><label>{labels_label}</label><input style='width : 270px' name='labels' id='labels_value' placeholder='Alpha, Beta, Gamma' value=''></fieldset>\n\
             <hr/>\n\
             <fieldset>\n\
             {share_btn}\n\
             {cancel_btn}\n\
             <span class='text-muted small'>{published_note}</span>\n\
             </fieldset>\n\
             </form>\n\
             </div>\n\
             </body></html>",
            title = title,
            url = url,
            csrf = csrf,
            share_btn = controls::submit_tag(&__("Share")),
            cancel_btn = controls::button_tag(&__("Cancel"), "", &[("onclick", "window.close()".to_string())]),
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
        let subscribe_title = __("Subscribe to feed...");
        let return_to_ttrss = __("Return to Tiny Tiny RSS");
        let multiple_urls = __("Multiple feed URLs found:");
        let subscribe_selected = __("Subscribe to selected feed");
        let edit_options = __("Edit subscription options");
        let login_label = __("Login:");
        let password_label = __("Password:");

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
                "<!DOCTYPE html><html><head><title>{subscribe_title}</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 {}\n\
                 <form action=\"public.php?return={}\" method=\"post\">\n\
                 <input type=\"hidden\" name=\"op\" value=\"login\">\n\
                 <fieldset><label>{login_label}</label><input name=\"login\" required=\"1\"></fieldset>\n\
                 <fieldset><label>{password_label}</label><input type=\"password\" name=\"password\" required=\"1\"></fieldset>\n\
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
                "<!DOCTYPE html><html><head><title>{subscribe_title}</title></head>\n\
                 <body class='flat ttrss_utility share_popup css_loading'>\n\
                 <div class='content'>\n\
                 <h1>{subscribe_title}</h1>\n\
                 <form method=\"post\" action='public.php'>\n\
                 <input type=\"hidden\" name=\"op\" value=\"bookmarklets--subscribe\">\n\
                 <input type=\"hidden\" name=\"csrf_token\" value=\"{csrf}\">\n\
                 <fieldset><label>Feed or site URL:</label><input style=\"width: 300px\" name=\"feed_url\" value=\"{feed_url_esc}\"></fieldset>\n\
                 <button class=\"alt-primary\" type=\"submit\">Subscribe</button>\n\
                 <a href=\"index.php\">{return_to_ttrss}</a>\n\
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
                             <fieldset><label style='display : inline'>{multiple_urls}</label>\
                             <select name='feed_url'>{options}</select></fieldset>\n\
                             <button class='alt-primary' type='submit'>{subscribe_selected}</button>\n\
                             <a href='index.php'>{return_to_ttrss}</a>\n\
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
                         <button class='alt-info' type='submit'>{edit_options}</button>\n\
                         <a href='index.php'>{return_to_ttrss}</a>\n\
                         </form>",
                    ));
                }
            }

            if code == 4 {
                // Already handled in the switch above.
            } else if out.is_empty() {
                out.push_str(&format!("<a href='index.php'>{return_to_ttrss}</a>"));
            }

            format!(
                "<!DOCTYPE html><html><head><title>{subscribe_title}</title></head>\n\
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
