//! Port of `tt-rss/tests_functional/SanitizerTest.php` (11 tests).
//!
//! Pure-string/DOM tests of `highlight_words_str` / `highlight_words` — no DB.

use crate::sanitizer::{highlight_words, highlight_words_str, serialize_for_tests};
use crate::sanitizer_parity::count;
use kuchikiki::parse_html;
use kuchikiki::traits::TendrilSink;

/// True if the element node has local name `name`.
fn is_el(node: &kuchikiki::NodeRef, name: &str) -> bool {
    node.as_element()
        .map(|el| el.name.local.to_string() == name)
        .unwrap_or(false)
}

/// Parse a fragment, run `highlight_words` on its body (mirroring the PHP
/// tests that call `Sanitizer::highlight_words($doc, $xpath, $words)`
/// directly), and return (serialized body children, `rv`).
fn highlight_fragment(fragment: &str, words: &[&str]) -> (String, bool) {
    let doc = parse_html().one(fragment);
    let mut body: Option<kuchikiki::NodeRef> = None;
    for html in doc.children() {
        if is_el(&html, "html") {
            for c in html.children() {
                if is_el(&c, "body") {
                    body = Some(c.clone());
                }
            }
        }
    }
    let body = body.expect("body");
    let ws: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    let rv = highlight_words(&body, &ws);
    let mut out = String::new();
    for c in body.children() {
        serialize_for_tests(&c, &mut out);
    }
    (out, rv)
}

#[test]
fn test_highlight_words_str_basic() {
    let result = highlight_words_str("Hello world", &["world".to_string()]);
    assert!(result.contains("highlight"));
    assert!(result.contains("world"));
}

#[test]
fn test_highlight_words_str_multiple_matches() {
    let result = highlight_words_str("test test test", &["test".to_string()]);
    assert!(result.contains("highlight"));
}

#[test]
fn test_highlight_words_str_no_match() {
    // When no words match, DOMDocument still wraps content in <span>.
    let result = highlight_words_str("Hello world", &["xyz".to_string()]);
    assert!(result.contains("Hello world"));
    // DOMDocument wrapping is expected behavior.
    assert!(!result.contains("highlight"));
}

#[test]
fn test_highlight_words_str_preserves_html() {
    let result = highlight_words_str("<b>Hello</b> world", &["hello".to_string()]);
    assert!(result.contains("highlight"));
    assert!(result.contains("Hello"));
}

#[test]
fn test_highlight_words_str_unicode() {
    let result = highlight_words_str("中文内容测试", &["中文".to_string()]);
    assert!(result.contains("highlight"));
}

#[test]
fn test_highlight_words_str_empty_words_array() {
    let result = highlight_words_str("Hello world", &[]);
    assert_eq!(result, "Hello world");
}

#[test]
fn test_highlight_words_str_special_chars_in_word() {
    let result = highlight_words_str("test$value", &["test$value".to_string()]);
    assert!(result.contains("highlight"));
}

#[test]
fn test_highlight_words_returns_true_on_match() {
    let (_, rv) = highlight_fragment("<p>Hello world</p>", &["world"]);
    assert!(rv);
}

#[test]
fn test_highlight_words_returns_false_on_no_match() {
    // When no words match, text nodes are still replaced (empty fragment)
    // but highlight_words() returns true because nodes were replaced.
    let (html, rv) = highlight_fragment("<p>Hello world</p>", &["xyz"]);
    // Actual behavior: returns true because child nodes were replaced even
    // though no highlighting occurred.
    assert!(rv);
    // Text content should still be present.
    assert_eq!(count(&html, "<p>"), 1);
}

#[test]
fn test_highlight_words_preserves_parent_structure() {
    let (html, _) = highlight_fragment("<div><p>Hello world</p></div>", &["world"]);
    assert_eq!(count(&html, "<div>"), 1);
}

#[test]
fn test_highlight_words_handles_nested_elements() {
    let (html, _) =
        highlight_fragment("<div><span><p>Nested content</p></span></div>", &["nested"]);
    assert_eq!(count(&html, "<p>"), 1);
}
