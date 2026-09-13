//! Port of `tt-rss/tests/mocked/SanitizerUnitTest.php` (81 tests).
//!
//! PHP mock context: `$_SESSION = []`, `Prefs::get` → false, no plugins,
//! `Config::get_self_url()` → `http://localhost/tt-rss` (not https).

use super::{count, san, san_hw, san_sandbox, san_site};

#[test]
fn test_sanitize_basic_html() {
    let input = "<p>This is a <strong>test</strong> paragraph.</p>";
    let result = san(input);
    assert!(result.contains("<p>This is a <strong>test</strong> paragraph.</p>"));
}

#[test]
fn test_sanitize_removes_script_tags() {
    let input = "<p>Safe content</p><script>alert(\"xss\")</script>";
    let result = san(input);
    assert!(!result.contains("<script>"));
    assert!(!result.contains("alert"));
    assert!(result.contains("Safe content"));
}

#[test]
fn test_sanitize_removes_onclick_attributes() {
    let input = "<a href=\"#\" onclick=\"alert('xss')\">Click me</a>";
    let result = san(input);
    assert!(!result.contains("onclick"));
    assert!(result.contains("Click me"));
}

#[test]
fn test_sanitize_removes_javascript_href() {
    let input = "<a href=\"javascript:alert('xss')\">Click me</a>";
    let result = san(input);
    assert!(!result.contains("javascript:"));
}

#[test]
fn test_sanitize_removes_data_attributes() {
    let input = "<div data-custom=\"value\">Content</div>";
    let result = san(input);
    assert!(!result.contains("data-custom"));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_removes_style_attribute() {
    let input = "<p style=\"color: red;\">Styled text</p>";
    let result = san(input);
    assert!(!result.contains("style="));
    assert!(result.contains("Styled text"));
}

#[test]
fn test_sanitize_removes_id_attribute() {
    let input = "<div id=\"myid\">Content</div>";
    let result = san(input);
    assert!(!result.contains("id="));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_removes_class_attribute() {
    let input = "<div class=\"myclass\">Content</div>";
    let result = san(input);
    assert!(!result.contains("class=\"myclass\""));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_removes_srcdoc_attribute() {
    let input = "<iframe src=\"url\" srcdoc=\"&lt;script&gt;parent.eval(&quot;alert('XSS | '+document.cookie)&quot;)&lt;/script&gt;\"></iframe>";
    let result = san(input);
    assert!(!result.contains("srcdoc"));
}

#[test]
fn test_sanitize_adds_noopener_noreferrer_to_links() {
    let input = "<a href=\"https://example.com\">Link</a>";
    let result = san(input);
    assert!(result.contains("rel=\"noopener noreferrer\""));
}

#[test]
fn test_sanitize_adds_target_blank_to_links() {
    let input = "<a href=\"https://example.com\">Link</a>";
    let result = san(input);
    assert!(result.contains("target=\"_blank\""));
}

#[test]
fn test_sanitize_adds_referrerpolicy_to_images() {
    let input = "<img src=\"https://example.com/image.jpg\" alt=\"Test\">";
    let result = san(input);
    assert!(result.contains("referrerpolicy=\"no-referrer\""));
}

#[test]
fn test_sanitize_adds_lazy_loading_to_images() {
    let input = "<img src=\"https://example.com/image.jpg\" alt=\"Test\">";
    let result = san(input);
    assert!(result.contains("loading=\"lazy\""));
}

#[test]
fn test_sanitize_rewrites_relative_urls() {
    let input = "<a href=\"/page.html\">Link</a>";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("https://example.com/page.html"));
}

#[test]
fn test_sanitize_rewrites_relative_image_src() {
    let input = "<img src=\"/image.jpg\" alt=\"Test\">";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("https://example.com/image.jpg"));
}

#[test]
fn test_sanitize_empty_string() {
    assert_eq!(san(""), "");
}

#[test]
fn test_sanitize_whitespace_only() {
    assert_eq!(san("   "), "");
}

#[test]
fn test_sanitize_allows_safe_html_elements() {
    let safe_elements = [
        "<p>Paragraph</p>",
        "<strong>Bold</strong>",
        "<em>Italic</em>",
        "<ul><li>List item</li></ul>",
        "<h1>Heading</h1>",
        "<blockquote>Quote</blockquote>",
        "<code>Code</code>",
        "<pre>Preformatted</pre>",
    ];
    for element in safe_elements {
        let result = san(element);
        assert!(!result.is_empty());
    }
}

#[test]
fn test_sanitize_table_elements() {
    let input = "<table><thead><tr><th>Header</th></tr></thead><tbody><tr><td>Data</td></tr></tbody></table>";
    let result = san(input);
    for tag in [
        "<table>",
        "<thead>",
        "<tbody>",
        "<tr>",
        "<th>Header</th>",
        "<td>Data</td>",
    ] {
        assert!(result.contains(tag), "missing {tag}: {result}");
    }
}

#[test]
fn test_sanitize_removes_embed_tag() {
    let input = "<p>Safe</p><embed src=\"file.swf\">";
    let result = san(input);
    assert!(!result.contains("<embed"));
    assert!(result.contains("Safe"));
}

#[test]
fn test_sanitize_removes_object_tag() {
    let input = "<p>Safe</p><object data=\"file.pdf\"></object>";
    let result = san(input);
    assert!(!result.contains("<object"));
    assert!(result.contains("Safe"));
}

#[test]
fn test_sanitize_wraps_iframes_in_div() {
    let input = "<iframe src=\"https://example.com\"></iframe>";
    // $_SESSION['hasSandbox'] = true;
    let result = san_sandbox(input);
    assert!(result.contains("<div class=\"embed-responsive\">"));
    assert!(result.contains("<iframe"));
}

#[test]
fn test_sanitize_iframe_without_sandbox_session() {
    let input = "<iframe src=\"https://example.com\"></iframe>";
    // unset($_SESSION['hasSandbox']);
    let result = san(input);
    // Without hasSandbox, iframe should be removed.
    assert!(!result.contains("<iframe"));
}

#[test]
fn test_sanitize_video_element() {
    let input = "<video poster=\"/poster.jpg\"><source src=\"/video.mp4\"></video>";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("<video"));
    assert!(result.contains("https://example.com/poster.jpg"));
    assert!(result.contains("<source"));
}

#[test]
fn test_sanitize_audio_element() {
    let input = "<audio><source src=\"/audio.mp3\"></audio>";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("<audio"));
    assert!(result.contains("<source"));
}

#[test]
fn test_sanitize_html5_semantic_elements() {
    let input = "<article><header><h1>Title</h1></header><section><p>Content</p></section><footer>Footer</footer></article>";
    let result = san(input);
    for tag in ["<article>", "<header>", "<section>", "<footer>"] {
        assert!(result.contains(tag), "missing {tag}: {result}");
    }
}

#[test]
fn test_sanitize_preserves_nested_structure() {
    let input = "<div><p>Level 1<span>Level 2<strong>Level 3</strong></span></p></div>";
    let result = san(input);
    assert!(result.contains("Level 1"));
    assert!(result.contains("Level 2"));
    assert!(result.contains("Level 3"));
}

#[test]
fn test_highlight_words_str_single_word() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a test string";
    let words = vec!["test".to_string()];
    let result = highlight_words_str(str_, &words);
    assert!(result.contains("<span class=\"highlight\">test</span>"));
    assert!(result.contains("This is a"));
}

#[test]
fn test_highlight_words_str_multiple_words() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a test string";
    let words = vec!["test".to_string(), "string".to_string()];
    let result = highlight_words_str(str_, &words);
    assert!(result.contains("<span class=\"highlight\">test</span>"));
    assert!(result.contains("<span class=\"highlight\">string</span>"));
}

#[test]
fn test_highlight_words_str_case_insensitive() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a TEST string";
    let words = vec!["test".to_string()];
    let result = highlight_words_str(str_, &words);
    assert!(result.contains("<span class=\"highlight\">TEST</span>"));
}

#[test]
fn test_highlight_words_str_empty_words() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a test string";
    let result = highlight_words_str(str_, &[]);
    assert_eq!(result, str_);
}

#[test]
fn test_highlight_words_str_word_not_found() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a test string";
    let words = vec!["notfound".to_string()];
    let result = highlight_words_str(str_, &words);
    // Even when no words match, the string is wrapped in a span tag.
    assert!(result.contains(str_));
    assert!(!result.contains("highlight"));
}

#[test]
fn test_highlight_words_str_multibyte_characters() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "This is a tëst string";
    let words = vec!["tëst".to_string()];
    let result = highlight_words_str(str_, &words);

    // NOTE: the PHP oracle (libxml 2.13.9) entity-encodes `ë` as `&euml;`
    // in `saveHTML`. The ported serializer matches libxml (see
    // `sanitizer::push_html_char`), so the oracle's actual output is the
    // entity form — *not* the raw char the PHP test expects for
    // `LIBXML_VERSION >= 21200` (that branch is wrong on this libxml).
    assert!(result.contains("class=\"highlight\""));
    assert!(result.contains("t&euml;st"));
}

#[test]
fn test_highlight_words_str_multiple_occurrences() {
    use crate::sanitizer::highlight_words_str;
    let str_ = "test and test again";
    let words = vec!["test".to_string()];
    let result = highlight_words_str(str_, &words);
    assert_eq!(count(&result, "<span class=\"highlight\">test</span>"), 2);
}

#[test]
fn test_sanitize_with_highlight_words() {
    let input = "<p>This is a test paragraph.</p>";
    let result = san_hw(input, &["test"]);
    assert!(result.contains("<span class=\"highlight\">test</span>"));
}

#[test]
fn test_sanitize_removes_form_elements() {
    let input = "<form><input type=\"text\" name=\"test\"><button>Submit</button></form>";
    let result = san(input);
    assert!(!result.contains("<form"));
    assert!(!result.contains("<input"));
    assert!(!result.contains("<button"));
}

#[test]
fn test_sanitize_removes_meta_tags() {
    let input = "<p>Content</p><meta http-equiv=\"refresh\" content=\"0;url=https://evil.com\">";
    let result = san(input);
    assert!(!result.contains("<meta"));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_removes_link_tags() {
    let input = "<p>Content</p><link rel=\"stylesheet\" href=\"evil.css\">";
    let result = san(input);
    assert!(!result.contains("<link"));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_removes_base_tag() {
    let input = "<p>Content</p><base href=\"https://evil.com\">";
    let result = san(input);
    assert!(!result.contains("<base"));
    assert!(result.contains("Content"));
}

#[test]
fn test_sanitize_complex_xss_attempt() {
    let input = "<img src=x onerror=\"alert('XSS')\" onload=\"alert('XSS2')\">";
    let result = san(input);
    assert!(!result.contains("onerror"));
    assert!(!result.contains("onload"));
    assert!(!result.contains("alert"));
}

#[test]
fn test_sanitize_svg_with_script() {
    let input = "<svg><script>alert(\"xss\")</script></svg>";
    let result = san(input);
    // SVG is not in allowed elements, so it should be removed.
    assert!(!result.contains("<svg"));
    assert!(!result.contains("alert"));
}

#[test]
fn test_sanitize_preserves_entities() {
    let input = "<p>&lt;script&gt;alert(&quot;test&quot;)&lt;/script&gt;</p>";
    let result = san(input);
    assert!(result.contains("&lt;"));
    assert!(result.contains("&gt;"));
    // &quot; gets converted to regular quotes by DOMDocument.
    assert!(result.contains("alert"));
}

#[test]
fn test_sanitize_unicode_characters() {
    let input = "<p>Hello 世界 🌍</p>";
    let result = san(input);
    assert!(result.contains("Hello"));
    // Content preserved (Rust emits raw UTF-8; PHP libxml emits numeric
    // character references — the PHP regex accepts either).
    assert!(
        result.contains("世界") || regex::Regex::new(r"&#\d+;").unwrap().is_match(&result),
        "got: {result}"
    );
}

#[test]
fn test_sanitize_long_content() {
    let input = format!("<p>{}</p>", "Lorem ipsum dolor sit amet. ".repeat(1000));
    let result = san(&input);
    assert!(result.len() > 1000);
}

#[test]
fn test_sanitize_deeply_nested_structure() {
    let input = "<div><div><div><div><div><p>Deep content</p></div></div></div></div></div>";
    let result = san(input);
    assert!(result.contains("Deep content"));
}

#[test]
fn test_sanitize_malformed_html() {
    let input = "<p>Unclosed paragraph<div>Mixed tags</p></div>";
    let result = san(input);
    assert!(result.contains("Unclosed paragraph"));
    assert!(result.contains("Mixed tags"));
}

#[test]
fn test_sanitize_picture_element_with_srcset() {
    let input = "<picture><source srcset=\"/image-large.jpg 1000w, /image-small.jpg 500w\"><img src=\"/image.jpg\"></picture>";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("<picture>"));
    assert!(result.contains("https://example.com/image-large.jpg"));
    assert!(result.contains("https://example.com/image-small.jpg"));
}

#[test]
fn test_sanitize_with_protocol_relative_url() {
    let input = "<a href=\"//example.com/page\">Link</a>";
    let result = san_site(input, "https://example.com");
    assert!(result.contains("href="));
}

#[test]
fn test_sanitize_abbreviation_and_acronym() {
    let input = "<p><abbr title=\"Hypertext Markup Language\">HTML</abbr> and <acronym title=\"Cascading Style Sheets\">CSS</acronym></p>";
    let result = san(input);
    assert!(result.contains("<abbr"));
    assert!(result.contains("<acronym"));
    // title attribute should be preserved (not in disallowed_attributes).
    assert!(result.contains("title="));
}

#[test]
fn test_sanitize_details_and_summary() {
    let input = "<details><summary>Click to expand</summary><p>Hidden content</p></details>";
    let result = san(input);
    assert!(result.contains("<details>"));
    assert!(result.contains("<summary>"));
    assert!(result.contains("Click to expand"));
    assert!(result.contains("Hidden content"));
}

#[test]
fn test_sanitize_kbd_and_samp() {
    let input = "<p>Press <kbd>Ctrl+C</kbd> to see <samp>output</samp></p>";
    let result = san(input);
    assert!(result.contains("<kbd>"));
    assert!(result.contains("<samp>"));
}

#[test]
fn test_sanitize_ruby_annotation() {
    let input = "<ruby>漢<rp>(</rp><rt>kan</rt><rp>)</rp></ruby>";
    let result = san(input);
    assert!(result.contains("<ruby>"));
    assert!(result.contains("<rt>"));
    assert!(result.contains("<rp>"));
}

#[test]
fn test_sanitize_blocks_localhost_in_img_src() {
    let input = "<p>Test <img src=\"http://127.0.0.1:8080/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(!result.contains("<img"));
    assert!(result.contains("&lt;img"));
    assert!(result.contains("127.0.0.1:8080"));
}

#[test]
fn test_sanitize_allows_non_standard_port_on_external_host_in_img_src() {
    let input = "<p>Test <img src=\"http://example.org:8080/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("http://example.org:8080/image.jpg"));
}

#[test]
fn test_sanitize_allows_private_ip_on_standard_http_port() {
    let input = "<p>Test <img src=\"http://192.168.1.100/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("http://192.168.1.100/image.jpg"));
}

#[test]
fn test_sanitize_allows_private_ip_on_standard_https_port() {
    let input = "<p>Test <img src=\"https://10.0.0.5/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("https://10.0.0.5/image.jpg"));
}

#[test]
fn test_sanitize_blocks_private_ip_on_non_standard_port() {
    let input = "<p>Test <img src=\"http://192.168.1.1:8080/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(!result.contains("<img"));
    assert!(result.contains("&lt;img"));
    assert!(result.contains("http://192.168.1.1:8080/image.jpg"));
}

#[test]
fn test_sanitize_blocks_link_local_on_standard_port() {
    let input =
        "<p>Test <img src=\"http://169.254.169.254/latest/meta-data/\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(!result.contains("<img"));
    assert!(result.contains("&lt;img"));
    assert!(result.contains("http://169.254.169.254/latest/meta-data/"));
}

#[test]
fn test_sanitize_allows_standard_port_in_img_src() {
    let input = "<p>Test <img src=\"http://example.org/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("http://example.org/image.jpg"));
}

#[test]
fn test_sanitize_allows_https_standard_port_in_img_src() {
    let input = "<p>Test <img src=\"https://example.org/image.jpg\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("https://example.org/image.jpg"));
}

#[test]
fn test_sanitize_preserves_data_uri_in_img_src() {
    let input = "<p>Test <img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"test\"> image</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("data:image/png;base64"));
}

#[test]
fn test_sanitize_preserves_href_with_non_standard_port() {
    let input = "<p>Test <a href=\"http://example.org:8080/article\">link</a> here</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<a"));
    assert!(result.contains("http://example.org:8080/article"));
    assert!(result.contains("link"));
}

#[test]
fn test_sanitize_preserves_href_with_localhost() {
    let input = "<p>Test <a href=\"http://localhost:3000/dev\">link</a> here</p>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<a"));
    assert!(result.contains("http://localhost:3000/dev"));
    assert!(result.contains("link"));
}

#[test]
fn test_sanitize_allows_non_standard_port_on_external_host_in_video_poster() {
    let input =
        "<video poster=\"http://example.org:8080/poster.jpg\"><source src=\"video.mp4\"></video>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<video"));
    assert!(result.contains("poster=\"http://example.org:8080/poster.jpg\""));
}

#[test]
fn test_sanitize_allows_standard_port_in_video_poster() {
    let input =
        "<video poster=\"https://example.org/poster.jpg\"><source src=\"video.mp4\"></video>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<video"));
    assert!(result.contains("poster=\"https://example.org/poster.jpg\""));
}

#[test]
fn test_sanitize_removes_localhost_poster_attribute() {
    let input =
        "<video poster=\"http://127.0.0.1:8080/poster.jpg\"><source src=\"video.mp4\"></video>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<video"));
    assert!(!result.contains("poster="));
    assert!(result.contains("<source"));
}

#[test]
fn test_sanitize_allows_private_ip_poster_on_standard_port() {
    let input =
        "<video poster=\"http://192.168.1.50/poster.jpg\"><source src=\"video.mp4\"></video>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<video"));
    assert!(result.contains("poster=\"http://192.168.1.50/poster.jpg\""));
}

#[test]
fn test_sanitize_removes_private_ip_poster_on_non_standard_port() {
    let input =
        "<video poster=\"http://10.0.0.5:8080/poster.jpg\"><source src=\"video.mp4\"></video>";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<video"));
    assert!(!result.contains("poster="));
    assert!(result.contains("<source"));
}

#[test]
fn test_sanitize_blocks_localhost_in_srcset() {
    let input = "<img srcset=\"http://127.0.0.1:8080/img1.jpg 1x, http://127.0.0.1:8080/img2.jpg 2x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(!result.contains("srcset="));
}

#[test]
fn test_sanitize_filters_invalid_urls_from_srcset() {
    let input = "<img srcset=\"https://example.org/img1.jpg 1x, http://localhost:8080/img2.jpg 2x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("srcset="));
    assert!(result.contains("https://example.org/img1.jpg"));
    assert!(!result.contains("localhost:8080"));
}

#[test]
fn test_sanitize_relative_url_with_base_url_port_preserved_in_href() {
    let input = "<p><a href=\"article.html\">Read more</a></p>";
    let result = san_site(input, "http://example.org:8080/feed/");
    assert!(result.contains("<a"));
    assert!(result.contains("http://example.org:8080/feed/article.html"));
}

#[test]
fn test_sanitize_relative_url_with_base_url_non_standard_port_allowed_in_img() {
    let input = "<p><img src=\"image.jpg\" alt=\"test\"></p>";
    let result = san_site(input, "http://example.org:8080/feed/");
    assert!(result.contains("<img"));
    assert!(result.contains("http://example.org:8080/feed/image.jpg"));
}

#[test]
fn test_sanitize_img_with_valid_srcset_but_invalid_src() {
    let input = "<img src=\"http://127.0.0.1:8080/bad.jpg\" srcset=\"https://example.org/img1.jpg 1x, https://example.org/img2.jpg 2x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    // Element should be preserved because srcset is valid.
    assert!(result.contains("<img"));
    // Invalid src should be removed.
    assert!(!result.contains("127.0.0.1"));
    // Valid srcset should be preserved.
    assert!(result.contains("srcset="));
    assert!(result.contains("https://example.org/img1.jpg"));
    assert!(result.contains("https://example.org/img2.jpg"));
}

#[test]
fn test_sanitize_img_with_only_srcset_no_src() {
    let input = "<img srcset=\"https://example.org/img1.jpg 1x, https://example.org/img2.jpg 2x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    assert!(result.contains("<img"));
    assert!(result.contains("srcset="));
    assert!(result.contains("https://example.org/img1.jpg"));
    assert!(result.contains("https://example.org/img2.jpg"));
}

#[test]
fn test_sanitize_img_with_invalid_src_and_invalid_srcset() {
    let input = "<img src=\"http://127.0.0.1:8080/bad.jpg\" srcset=\"http://localhost:8080/img1.jpg 1x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    // Element should be replaced with escaped text.
    assert!(!result.contains("<img"));
    assert!(result.contains("&lt;img"));
    assert!(result.contains("127.0.0.1:8080"));
}

#[test]
fn test_sanitize_img_with_invalid_src_and_no_srcset() {
    let input = "<img src=\"http://127.0.0.1:8080/bad.jpg\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    // Element should be replaced with escaped text.
    assert!(!result.contains("<img"));
    assert!(result.contains("&lt;img"));
    assert!(result.contains("127.0.0.1:8080"));
}

#[test]
fn test_sanitize_source_with_valid_srcset_but_invalid_src() {
    let input = "<picture><source src=\"http://localhost:8080/bad.jpg\" srcset=\"https://example.org/img1.jpg 1x, https://example.org/img2.jpg 2x\"><img src=\"https://example.org/fallback.jpg\"></picture>";
    let result = san_site(input, "http://example.com");
    // Source element should be preserved because srcset is valid.
    assert!(result.contains("<source"));
    // Invalid src should be removed.
    assert!(!result.contains("localhost:8080"));
    // Valid srcset should be preserved.
    assert!(result.contains("srcset="));
    assert!(result.contains("https://example.org/img1.jpg"));
    assert!(result.contains("https://example.org/img2.jpg"));
}

#[test]
fn test_sanitize_source_with_only_srcset_no_src() {
    let input = "<picture><source srcset=\"https://example.org/img1.jpg 1x, https://example.org/img2.jpg 2x\"><img src=\"https://example.org/fallback.jpg\"></picture>";
    let result = san_site(input, "http://example.com");
    // Source element should be preserved (valid HTML - source can have
    // srcset without src).
    assert!(result.contains("<source"));
    assert!(result.contains("srcset="));
    assert!(result.contains("https://example.org/img1.jpg"));
    assert!(result.contains("https://example.org/img2.jpg"));
}

#[test]
fn test_sanitize_source_with_invalid_src_and_invalid_srcset() {
    let input = "<picture><source src=\"http://127.0.0.1:8080/bad.jpg\" srcset=\"http://localhost:8080/img1.jpg 1x\"><img src=\"https://example.org/fallback.jpg\"></picture>";
    let result = san_site(input, "http://example.com");
    // Source element should be replaced with escaped text.
    assert!(!result.contains("<source"));
    assert!(result.contains("&lt;source"));
    assert!(result.contains("127.0.0.1:8080"));
}

#[test]
fn test_sanitize_srcset_with_mixed_valid_and_invalid_urls() {
    let input = "<img srcset=\"https://example.org/good1.jpg 1x, http://localhost:8080/bad.jpg 2x, https://example.org/good2.jpg 3x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    // Element should be preserved because some srcset URLs are valid.
    assert!(result.contains("<img"));
    assert!(result.contains("srcset="));
    // Valid URLs should be preserved.
    assert!(result.contains("https://example.org/good1.jpg"));
    assert!(result.contains("https://example.org/good2.jpg"));
    // Invalid URL should be removed.
    assert!(!result.contains("localhost:8080"));
}

#[test]
fn test_sanitize_img_with_valid_src_and_partially_valid_srcset() {
    let input = "<img src=\"https://example.org/main.jpg\" srcset=\"https://example.org/good.jpg 1x, http://127.0.0.1:8080/bad.jpg 2x\" alt=\"test\">";
    let result = san_site(input, "http://example.com");
    // Element should be preserved.
    assert!(result.contains("<img"));
    // Valid src should be preserved.
    assert!(result.contains("src=\"https://example.org/main.jpg\""));
    // Valid srcset URL should be preserved.
    assert!(result.contains("https://example.org/good.jpg"));
    // Invalid srcset URL should be removed.
    assert!(!result.contains("127.0.0.1"));
}
