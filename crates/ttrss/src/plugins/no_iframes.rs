//! The `no_iframes` plugin (`tt-rss/plugins/no_iframes/init.php`, M7.6) —
//! removes embedded iframes from article content during sanitization, unless
//! a plugin hooks `HOOK_IFRAME_WHITELISTED` for the iframe's `src` host (the
//! only known use is `af_youtube_embed`, M7.11).
//!
//! PHP's `hook_sanitize` walks `//iframe` on the `DOMDocument` and removes
//! every node for which `Sanitizer::iframe_whitelisted` is false; the port's
//! hook takes the sanitized-fragment HTML (`&mut String`, the M7.1 locked
//! data surface) and performs the same walk as a string scan — the round-trip
//! happens once per `Sanitizer::sanitize` call, only when at least one active
//! plugin registers `HOOK_SANITIZE` (see the call site in
//! [`crate::sanitizer::sanitize`]).
//!
//! The reference's own `//iframe` pass (the port's `process_iframes`) runs
//! *before* `HOOK_SANITIZE` and only sets sandboxes; the removal is the
//! plugin's. With no whitelisting plugin active, every iframe is removed —
//! matching a reference install without `af_youtube_embed`.

use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::HOOK_IFRAME_WHITELISTED;
use crate::pluginhost::HOOK_SANITIZE;

/// PHP's `class No_Iframes extends Plugin`.
#[derive(Clone, Copy)]
pub struct NoIframes;

impl Plugin for NoIframes {
    fn name(&self) -> &'static str {
        "no_iframes"
    }
    fn class_name(&self) -> &'static str {
        "No_Iframes"
    }
    fn about(&self) -> About {
        About::new("Remove embedded iframes (unless whitelisted)", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_SANITIZE);
    }

    /// `No_Iframes::hook_sanitize` — remove every iframe whose `src` host is
    /// not whitelisted by `HOOK_IFRAME_WHITELISTED` (PHP:
    /// `Sanitizer::iframe_whitelisted` — no host means not whitelisted).
    /// `site_url`/`allowed_elements`/`disallowed_attributes`/`article_id`
    /// are part of the hook contract; this plugin ignores them, as PHP does.
    fn hook_sanitize<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        doc: &'a mut String,
        _site_url: &'a str,
        _allowed_elements: &'a [String],
        _disallowed_attributes: &'a [String],
        _article_id: i32,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut out = String::with_capacity(doc.len());
            let mut rest = doc.as_str();
            while let Some(pos) = rest.find("<iframe") {
                let tag_start = pos;
                let Some(rel) = rest[tag_start..].find('>') else {
                    break;
                };
                let tag_end = tag_start + rel;
                let tag = &rest[tag_start..=tag_end];
                // `<iframe` must be a real tag name (not `<iframeish`).
                let after_name = &tag[7..]; // `<iframe` is 7 chars
                if !matches!(
                    after_name.as_bytes().first(),
                    Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'>') | Some(b'/')
                ) {
                    rest = &rest[tag_start + 1..];
                    continue;
                }
                let src = tag_attr(tag, "src");
                let host_ = crate::sanitizer::url_host(&src);
                let whitelisted = if host_.is_empty() {
                    // PHP: `parse_url($src, PHP_URL_HOST)` empty → false.
                    false
                } else {
                    // `run_hooks_until(HOOK_IFRAME_WHITELISTED, true, host)`
                    // (PHP `Sanitizer::iframe_whitelisted`): the first
                    // whitelisting plugin decides. A direct loop over
                    // `hook_order` — the generic runner's per-call HRTB
                    // closure cannot borrow this per-iframe local.
                    let mut w = false;
                    for idx in host.hook_order(HOOK_IFRAME_WHITELISTED) {
                        if host
                            .host()
                            .plugin(idx)
                            .hook_iframe_whitelisted(host, &host_)
                            .await
                        {
                            w = true;
                            break;
                        }
                    }
                    w
                };
                if !whitelisted {
                    out.push_str(&rest[..tag_start]);
                    let after = &rest[tag_end + 1..];
                    // Drop the element body too (PHP removes the DOM node).
                    if let Some(close_rel) = after.find("</iframe") {
                        let mut end = close_rel + "</iframe".len();
                        while end < after.len() && after.as_bytes()[end] != b'>' {
                            end += 1;
                        }
                        rest = if end < after.len() { &after[end + 1..] } else { "" };
                    } else {
                        rest = after;
                    }
                } else {
                    out.push_str(&rest[..=tag_end]);
                    rest = &rest[tag_end + 1..];
                }
            }
            out.push_str(rest);
            *doc = out;
        })
    }
}

/// The value of attribute `name` in a start tag (`""` when absent), matching
/// PHP's `$entry->getAttribute("src")` on the serialized form.
fn tag_attr(tag: &str, name: &str) -> String {
    let needle = format!("{name}=");
    let Some(i) = tag.find(&needle) else {
        return String::new();
    };
    let v = &tag[i + needle.len()..];
    let v = v.trim_start();
    if let Some(c) = v.chars().next() {
        if c == '"' || c == '\'' {
            let q = c;
            let body = &v[1..];
            let Some(end) = body.find(q) else {
                return body.to_string();
            };
            return body[..end].to_string();
        }
    }
    v.split([' ', '\t', '\n', '\r', '>']).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata() {
        let p = NoIframes;
        assert_eq!(p.name(), "no_iframes");
        assert_eq!(p.class_name(), "No_Iframes");
        assert_eq!(p.about().description, "Remove embedded iframes (unless whitelisted)");
        assert_eq!(p.about().author, "fox");
        assert!(!p.about().system);
        assert_eq!(p.api_version(), 2);
    }

    #[test]
    fn attr_extraction() {
        assert_eq!(tag_attr(r#"<iframe src="https://x/y" wmode="opaque">"#, "src"), "https://x/y");
        assert_eq!(tag_attr(r#"<iframe src='a b' >"#, "src"), "a b");
        assert_eq!(tag_attr(r#"<iframe wmode="opaque" src=v>#"#, "src"), "v");
        assert_eq!(tag_attr(r#"<iframe wmode="opaque">"#, "src"), "");
        assert_eq!(tag_attr(r#"<iframe>"#, "src"), "");
        assert_eq!(tag_attr(r#"<iframe data-x="1" src="https://h/p?a=b" >"#, "src"), "https://h/p?a=b");
    }

    /// The removal walk: non-whitelisted iframes are dropped (with their
    /// body), everything else survives untouched. The whitelist chain is
    /// empty here (no whitelisting plugin registered), so every iframe goes.
    #[tokio::test]
    async fn removes_all_iframes_without_whitelist() {
        let host = PluginHost::startup(&[|| Box::new(NoIframes) as Box<dyn Plugin>]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();
        let mut ctx =
        HostCtx::for_user(&host, &pool, &test_cfg, Some(1), None, 0, &[], &["no_iframes"]);
        let plugin = ctx.get_plugin("no_iframes").expect("active");

        let mut doc = "<p>a</p><iframe src=\"https://y/x\" width=\"1\"></iframe><p>b</p><iframe src=\"https://y/2\"></iframe>".to_string();
        plugin
            .hook_sanitize(&mut ctx, &mut doc, "http://site/", &[], &[], 0)
            .await;
        assert_eq!(doc, "<p>a</p><p>b</p>");
    }

    /// A registered `HOOK_IFRAME_WHITELISTED` plugin keeps its host's
    /// iframes (M7.11's `af_youtube_embed` interlock).
    #[tokio::test]
    async fn keeps_whitelisted_iframes() {
        #[derive(Clone, Copy)]
        struct YtWhitelist;
        impl Plugin for YtWhitelist {
            fn name(&self) -> &'static str {
                "yt_wl"
            }
            fn class_name(&self) -> &'static str {
                "YtWl"
            }
            fn about(&self) -> About {
                About::new("test", "test")
            }
            fn api_version(&self) -> i32 {
                2
            }
            fn init(&self, host: &mut PluginHost) {
                host.add_hook_default(HOOK_IFRAME_WHITELISTED);
            }
            fn hook_iframe_whitelisted<'a, 'h: 'a>(
                &'a self,
                _host: &'a mut HostCtx<'h>,
                url: &'a str,
            ) -> BoxFuture<'a, bool> {
                let keep = url.contains("youtube");
                Box::pin(async move { keep })
            }
        }

        let host = PluginHost::startup(&[
            || Box::new(NoIframes) as Box<dyn Plugin>,
            || Box::new(YtWhitelist) as Box<dyn Plugin>,
        ]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();
        let mut ctx =
        HostCtx::for_user(&host, &pool, &test_cfg, Some(1), None, 0, &[], &["no_iframes", "yt_wl"]);
        let plugin = ctx.get_plugin("no_iframes").expect("active");

        let mut doc =
            "<iframe src=\"https://www.youtube.com/embed/1\"></iframe><iframe src=\"https://evil/x\"></iframe>"
                .to_string();
        plugin
            .hook_sanitize(&mut ctx, &mut doc, "http://site/", &[], &[], 0)
            .await;
        assert_eq!(
            doc,
            "<iframe src=\"https://www.youtube.com/embed/1\"></iframe>"
        );
    }
}
