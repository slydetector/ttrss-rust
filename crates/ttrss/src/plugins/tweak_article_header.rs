//! The `tweak_article_header` plugin — a pure-frontend tweak that moves the CDM
//! article header's feed name out of its right-aligned slot and inline, right
//! after the article title (the stock layout shows the feed name on the far
//! right in italics; this puts it immediately after the title, de-italicized,
//! with a small gap).
//!
//! It is **not** a bundled plugin in the frozen reference — it ships only in
//! this port — so its assets are defined here rather than `include_str!`'d
//! from `tt-rss/plugins/` (there is no reference file to stay byte-identical
//! with). `init` registers no hooks and `get_css` is empty; the whole effect
//! is `get_js`.
//!
//! What the JS does, per `.cdm .header`:
//! - moves the `.feed` element (a direct child of the header) into the
//!   `.titleWrap`, right after the `.title`;
//! - styles it `display:inline; margin-left:8px; font-style:normal` — set as
//!   individual properties so the feed's own inline `background-color` (the
//!   per-feed `feed_bg_color`) is preserved;
//! - wraps the feed **and** the `.author` in a `white-space:nowrap` span so
//!   they stay on one line. Without this, a long title wraps the author onto
//!   the next line while the feed stays on the title's last line, which looks
//!   misaligned (the user reported the feed/author drifting apart on
//!   long-title rows);
//! - for a single (non-vfeed) feed the `feed_title` is empty and the reference
//!   CSS already hides `.feed` (`[data-is-vfeed="false"] .header .feed` →
//!   `display:none`); a blanket inline `display:inline` would override that and
//!   leave an empty background box, so an empty feed is kept hidden instead of
//!   moved.
//!
//! It re-applies on dynamic renders via a `MutationObserver` on `document.body`
//! (headlines load async and re-render on feed switches / scroll), guarded by
//! a parent-class check so it is idempotent and never double-wraps.

use crate::pluginhost::host::PluginHost;
use crate::pluginhost::plugin::{About, Plugin};
use async_trait::async_trait;

/// The `tweak_article_header` plugin.
#[derive(Clone, Copy)]
pub struct TweakArticleHeader;

#[async_trait]
impl Plugin for TweakArticleHeader {
    fn name(&self) -> &'static str {
        "tweak_article_header"
    }
    fn class_name(&self) -> &'static str {
        "Tweak_Article_Header"
    }
    fn about(&self) -> About {
        About::new(
            "In the article header for special and category feeds, moves the feed title next to the article name.",
            "slydetector",
        )
    }
    fn api_version(&self) -> i32 {
        2
    }
    /// Pure frontend — no hooks, no backend methods.
    fn init(&self, _host: &mut PluginHost) {}

    fn get_js(&self) -> String {
        TWEAK_ARTICLE_HEADER_JS.to_string()
    }
}

/// The client-side JS: move the feed name inline after the title in every CDM
/// header, keep it baseline-aligned with the author, and re-apply on dynamic
/// renders.
const TWEAK_ARTICLE_HEADER_JS: &str = r#"/* tweak_article_header: move the CDM feed name inline, right after the article title. */
(function () {
	"use strict";

	// The feed counts as already-inlined once it sits inside our nowrap wrap.
	function isInlined(feed) {
		var p = feed.parentElement;
		return p && p.classList && p.classList.contains("tweak-article-header-wrap");
	}

	function processHeader(h) {
		var tw = h.querySelector(".titleWrap");
		var tit = tw && tw.querySelector(".title");
		var feed = h.querySelector(".feed");
		if (!tw || !tit || !feed) return;

		// A single (non-vfeed) feed has an empty feed_title: the reference CSS
		// already hides .feed there (`#headlines-frame[data-is-vfeed="false"]
		// .header .feed { display: none }`). Our inline `display:inline` below
		// would override that rule and leave an empty background box, so when
		// there's no name to show we keep the element hidden instead.
		if (!feed.textContent.trim()) {
			feed.style.display = "none";
			return;
		}

		if (isInlined(feed)) return;

		// Move the feed (a direct child of the header) into the titleWrap,
		// right after the title.
		if (!tw.contains(feed)) tw.insertBefore(feed, tit.nextSibling);

		// Individual properties (not cssText) so the feed's own inline
		// background-color — the per-feed feed_bg_color — is preserved.
		feed.style.display = "inline";
		feed.style.marginLeft = "8px";
		feed.style.fontStyle = "normal";

		// Keep the feed and the author together so a long title can't split
		// them across lines (which looked misaligned).
		var author = tw.querySelector(".author");
		if (author && author.textContent.trim()) {
			if (feed.nextSibling !== author) {
				feed.insertAdjacentElement("afterend", author);
			}
			var wrap = document.createElement("span");
			wrap.className = "tweak-article-header-wrap";
			wrap.style.whiteSpace = "nowrap";
			feed.parentNode.insertBefore(wrap, feed);
			wrap.appendChild(feed);
			wrap.appendChild(author);
		}
	}

	function processAll() {
		var headers = document.querySelectorAll(".cdm .header");
		for (var i = 0; i < headers.length; i++) processHeader(headers[i]);
	}

	function init() {
		processAll();
		// Headlines load async and re-render on feed switches / scroll, so
		// re-apply on any DOM change in the body. Debounced to one pass per
		// frame; idempotent via isInlined().
		var scheduled = false;
		var mo = new MutationObserver(function () {
			if (scheduled) return;
			scheduled = true;
			requestAnimationFrame(function () {
				scheduled = false;
				processAll();
			});
		});
		mo.observe(document.body, { childList: true, subtree: true });
	}

	if (document.readyState === "loading") {
		document.addEventListener("DOMContentLoaded", init);
	} else {
		init();
	}
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_js_is_nonempty_and_has_the_move() {
        let js = TweakArticleHeader.get_js();
        assert!(!js.is_empty());
        assert!(js.contains(".titleWrap"));
        assert!(js.contains("tweak-article-header-wrap"));
        assert!(js.contains("feed_bg_color") || js.contains("background-color"));
        assert!(js.contains("MutationObserver"));
    }

    #[test]
    fn get_js_hides_empty_feed() {
        // Single (non-vfeed) feeds have an empty feed_title; the plugin must
        // hide the element (display:none) rather than force it visible, or an
        // empty background box would show after the title.
        let js = TweakArticleHeader.get_js();
        assert!(
            js.contains("feed.textContent.trim()"),
            "empty-feed check missing from JS"
        );
        assert!(
            js.contains("display = \"none\""),
            "empty feed must be hidden, not left as an empty box"
        );
    }
}
