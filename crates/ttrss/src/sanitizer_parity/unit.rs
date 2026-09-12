//! Port of `tt-rss/tests/integration/SanitizerTest.php` (2 tests).

use super::san;

#[test]
fn test_sanitize_non_ascii() {
    // PHP (libxml2 `saveHTML`) serializes non-ASCII characters as numeric
    // character references — the ported serializer matches (see
    // `sanitizer::HTML_NAMED_ENTITIES` / `push_html_char`).
    let result = san("<p>中文</p>");
    assert_eq!(result, "<p>&#20013;&#25991;</p>");
}

#[test]
fn test_sanitize_keep_figure() {
    let result = san("<figure>Content</figure>");
    assert_eq!(result, "<figure>Content</figure>");
}
