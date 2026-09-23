//! `af_youtube_embed` (M7.11) — "Embed videos in Youtube RSS feeds (and
//! whitelist Youtube iframes)" (fox).
//!
//! Two hooks:
//! - [`HOOK_IFRAME_WHITELISTED`]: admits YouTube iframe hosts so the sanitizer
//!   gives them the loose PHP sandbox value (the M5a.3 residual).
//! - [`HOOK_RENDER_ENCLOSURE`]: renders a YouTube enclosure's `content_url` as
//!   an embedded-player iframe (the port of the reference's markup, verbatim).
//!
//! `UrlHelper::url_to_youtube_vid` is the port's [`crate::headlines::url_to_youtube_vid`].

use serde_json::Value;

use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::{HOOK_IFRAME_WHITELISTED, HOOK_RENDER_ENCLOSURE};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};

/// The `af_youtube_embed` plugin (the PHP's `Af_Youtube_Embed`).
#[derive(Clone, Copy)]
pub struct AfYoutubeEmbed;

/// The YouTube hosts the reference whitelists (the PHP's `hook_iframe_whitelisted`).
const YOUTUBE_HOSTS: &[&str] = &[
    "www.youtube.com",
    "youtube.com",
    "www.youtube-nocookie.com",
    "youtube-nocookie.com",
    "youtu.be",
];

impl Plugin for AfYoutubeEmbed {
    fn name(&self) -> &'static str {
        "af_youtube_embed"
    }
    fn class_name(&self) -> &'static str {
        "Af_Youtube_Embed"
    }
    fn about(&self) -> About {
        About::new("Embed videos in Youtube RSS feeds (and whitelist Youtube iframes)", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_RENDER_ENCLOSURE);
        host.add_hook_default(HOOK_IFRAME_WHITELISTED);
    }

    /// `hook_iframe_whitelisted($src)` — `$src` is the iframe's src *host* (the
    /// sanitizer extracts it); true when it is a YouTube host.
    fn hook_iframe_whitelisted<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        src: &'a str,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move { YOUTUBE_HOSTS.contains(&src) })
    }

    /// `hook_render_enclosure($entry, $id, $rv)` — render a YouTube enclosure as
    /// an embedded-player iframe, or `""` when the `content_url` is not YouTube
    /// (the caller then falls back to the default enclosure entry).
    fn hook_render_enclosure<'a, 'h: 'a>(
        &'a self,
        _host: &'a mut HostCtx<'h>,
        entry: &'a Value,
        _article_id: i32,
        _rv: &'a Value,
    ) -> BoxFuture<'a, String> {
        Box::pin(async move {
            let url = entry.get("content_url").and_then(Value::as_str).unwrap_or("");
            match crate::headlines::url_to_youtube_vid(url) {
                Some(vid) => embed_markup(&vid),
                None => String::new(),
            }
        })
    }
}

/// The reference's embedded-player markup (verbatim, with the video id).
fn embed_markup(vid_id: &str) -> String {
    // The reference's markup carries LITERAL TAB indentation (its source
    // file is tab-indented inside the string) — `\t` escapes would render
    // as the two-character text `\t` and break byte-parity.
    format!(
        "<div class='embed-responsive'>\n\
         \t\t\t\t<iframe class='youtube-player'\n\
         \t\t\t\t\ttype='text/html' width='640' height='385'\n\
         \t\t\t\t\ttitle='YouTube video player'\n\
         \t\t\t\t\tsrc=\"https://www.youtube-nocookie.com/embed/{vid_id}\"\n\
         \t\t\t\t\treferrerpolicy='strict-origin-when-cross-origin'\n\
         \t\t\t\t\tallow='accelerometer; autoplay; clipboard-write; encrypted-media; fullscreen *; gyroscope; picture-in-picture; web-share'\n\
         \t\t\t\t\tsandbox='allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox'\n\
         \t\t\t\t\tframeborder='0'></iframe>\n\
         \t\t\t\t</div>"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    /// A host with only `af_youtube_embed` + a lazy pool (these hooks never touch
    /// the DB).
    fn setup() -> (PluginHost, sqlx::postgres::PgPool, crate::core::config::Config) {
        let host = PluginHost::startup(&[|| Box::new(AfYoutubeEmbed)]);
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        (host, pool, crate::core::config::Config::for_test())
    }

    /// The YouTube host whitelist (the PHP's `in_array`).
    #[tokio::test]
    async fn iframe_whitelisted_is_youtube_hosts_only() {
        let (host, pool, test_cfg) = setup();
        let p = AfYoutubeEmbed;
        for h in YOUTUBE_HOSTS {
            let mut ctx = HostCtx::empty(&host, &pool, &test_cfg);
            assert!(p.hook_iframe_whitelisted(&mut ctx, h).await, "should whitelist {h}");
        }
        let mut ctx = HostCtx::empty(&host, &pool, &test_cfg);
        assert!(!p.hook_iframe_whitelisted(&mut ctx, "vimeo.com").await);
        assert!(!p.hook_iframe_whitelisted(&mut ctx, "example.com").await);
    }

    /// `hook_render_enclosure` renders a YouTube `content_url` as the embedded
    /// player (src/sandbox/allow) and `""` otherwise.
    #[tokio::test]
    async fn render_enclosure_youtube_vs_other() {
        let (host, pool, test_cfg) = setup();
        let p = AfYoutubeEmbed;
        let mut ctx = HostCtx::empty(&host, &pool, &test_cfg);
        let yt = json!({ "content_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ" });
        let out = p.hook_render_enclosure(&mut ctx, &yt, 1, &json!({})).await;
        assert!(out.contains("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"));
        assert!(out.contains("class='youtube-player'"));
        assert!(out.contains(
            "sandbox='allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox'"
        ));
        assert!(out.contains("frameborder='0'"));

        let not_yt = json!({ "content_url": "https://vimeo.com/12345" });
        let out = p.hook_render_enclosure(&mut ctx, &not_yt, 1, &json!({})).await;
        assert_eq!(out, "");
    }

    #[test]
    fn metadata() {
        let p = AfYoutubeEmbed;
        assert_eq!(p.name(), "af_youtube_embed");
        assert_eq!(p.class_name(), "Af_Youtube_Embed");
        assert_eq!(p.api_version(), 2);
        assert_eq!(
            p.about().description,
            "Embed videos in Youtube RSS feeds (and whitelist Youtube iframes)"
        );
        assert_eq!(p.about().author, "fox");
    }

    /// M5a.3 residual (M7.11): a YouTube iframe keeps the loose PHP sandbox
    /// value when the plugin is active — the `HOOK_IFRAME_WHITELISTED` hook is
    /// honored by the read-side `sanitize` (the admitted-host set reaches the
    /// core's iframe pass).
    #[tokio::test]
    async fn youtube_iframe_keeps_loose_sandbox() {
        let host =
            crate::pluginhost::PluginHost::startup(&crate::pluginhost::all_plugins());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();
        let mut ctx =
            HostCtx::for_user(&host, &pool, &test_cfg, Some(1), None, 0, &[], &["af_youtube_embed"]);
        let html =
            "<div><iframe src=\"http://www.youtube.com/embed/abc123\" width=\"400\"></iframe></div>";
        let out = crate::sanitizer::sanitize(
            html,
            false,
            false,
            true,
            false,
            None,
            None,
            0,
            Some(&mut ctx),
        )
        .await;
        assert!(
            out.contains("sandbox=\"allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox\""),
            "got: {out}"
        );
    }

    /// The whitelist is selective: a non-YouTube iframe gets the restrictive
    /// sandbox even with the plugin active.
    #[tokio::test]
    async fn non_youtube_iframe_gets_restrictive_sandbox() {
        let host =
            crate::pluginhost::PluginHost::startup(&crate::pluginhost::all_plugins());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        let test_cfg = crate::core::config::Config::for_test();
        let mut ctx =
            HostCtx::for_user(&host, &pool, &test_cfg, Some(1), None, 0, &[], &["af_youtube_embed"]);
        let html = "<div><iframe src=\"http://example.com/embed\" width=\"400\"></iframe></div>";
        let out = crate::sanitizer::sanitize(
            html,
            false,
            false,
            true,
            false,
            None,
            None,
            0,
            Some(&mut ctx),
        )
        .await;
        assert!(out.contains("sandbox=\"allow-scripts\""), "got: {out}");
    }

    /// Port of the PHP `test_iframe_whitelisted_returns_false_with_no_src` — an
    /// empty src can't be a whitelisted iframe. (The PHP
    /// `test_iframe_whitelisted_runs_hooks` — a mock plugin whitelisting a host —
    /// is covered end-to-end by `youtube_iframe_keeps_loose_sandbox`: the hook
    /// fires and its result is honored.)
    #[tokio::test]
    async fn iframe_whitelisted_empty_src_is_false() {
        let (host, pool, test_cfg) = setup();
        let mut ctx = HostCtx::empty(&host, &pool, &test_cfg);
        assert!(!crate::sanitizer::iframe_whitelisted(&mut ctx, "").await);
    }
}
