//! Port of `tt-rss/tests/integration/SanitizerTest.php` (2 tests).

use super::san;

#[test]
fn test_sanitize_non_ascii() {
    // NOTE: PHP (libxml2 saveHTML) serializes non-ASCII characters as numeric
    // character references: `<p>&#20013;&#25991;</p>`. The Rust serializer
    // emits raw UTF-8, which is the same document semantically.
    let result = san("<p>中文</p>");
    assert_eq!(result, "<p>中文</p>");
}

#[test]
fn test_sanitize_keep_figure() {
    let result = san("<figure>Content</figure>");
    assert_eq!(result, "<figure>Content</figure>");
}
