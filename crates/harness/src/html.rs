//! Structural HTML comparison.
//!
//! Rather than regex-munging HTML text (fragile around inline `<script>`/
//! `<style>`, attribute values, and comments), we parse both documents with the
//! canonical `html5ever`/`markup5ver` parser (via the `scraper` front-end) and
//! compare **canonical DOM forms**:
//!
//!   * whitespace-only text nodes are dropped (the "insignificant spacing" we
//!     don't want to chase),
//!   * text nodes are trimmed and value-normalized (cache-bust, hex, timestamps),
//!   * element attributes are sorted by name and value-normalized,
//!   * `<script>`/`<style>` raw text is kept as a (normalized) text node, so JS
//!     and CSS content is compared faithfully instead of being mangled.
//!
//! This compares *structure + meaningful values*, which is what "equivalent
//! HTML" actually means.

use ego_tree::NodeRef;
use scraper::{node::Node, Html};
use std::sync::LazyLock;

use crate::normalize;

/// Heuristic: does this body look like an HTML document (vs plain text / JSON)?
pub fn is_html(s: &str) -> bool {
    let t = s.trim_start().to_ascii_lowercase();
    t.starts_with("<!doctype html") || t.starts_with("<html") || t.starts_with("<body")
}

/// Matches an HTML/XML opening tag: `<` + a letter name + anything-but-`<>` + `>`.
/// (A bare `<` followed by a non-letter, e.g. `a < 5`, does not match.)
static FRAGMENT_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<[a-zA-Z][^<>]*>").unwrap());

/// Heuristic: does this JSON *string field* hold an HTML fragment (article
/// body, plugin buttons, ...) as opposed to plain text? HTML fields are
/// compared structurally (see `canonical_html`); plain text is value-normalized.
pub fn is_html_fragment(s: &str) -> bool {
    FRAGMENT_TAG.is_match(s)
}

/// Produce a canonical, normalized serialization of an HTML document for
/// structural comparison. Two documents that differ only in inter-tag
/// whitespace, attribute order, cache-bust timestamps, and other normalized
/// values produce identical canonical forms.
pub fn canonical_html(s: &str) -> String {
    let html = Html::parse_document(s);
    let mut out = String::new();
    canonicalize_node(html.tree.root(), &mut out);
    out
}

/// Concatenate all text under a node (for `<script>`/`<style>` code blocks).
fn collect_text(node: NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(t.text.as_ref()),
        _ => {
            for child in node.children() {
                collect_text(child, out);
            }
        }
    }
}

/// Collapse runs of whitespace (space/tab/newline) to a single space and trim.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

fn canonicalize_node(node: NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Element(el) => {
            let name = el.name();
            out.push('<');
            out.push_str(name);
            // Sort attributes by name and normalize their values so attribute
            // order and volatile values (cache-bust, timestamps, hex) don't
            // cause spurious differences.
            let mut attrs: Vec<(String, String)> = el
                .attrs()
                .map(|(k, v)| (k.to_string(), normalize::normalize_string(v)))
                .collect();
            attrs.sort();
            for (k, v) in &attrs {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(v);
                out.push('"');
            }
            out.push('>');
            if name == "script" || name == "style" {
                // Code blocks: compare the content faithfully but ignore
                // indentation/line-break differences (tabs vs spaces), which
                // are cosmetic. A real token change still shows up.
                let mut text = String::new();
                collect_text(node, &mut text);
                let norm = normalize::normalize_string(&collapse_ws(&text));
                if !norm.is_empty() {
                    out.push_str(&norm);
                }
            } else {
                for child in node.children() {
                    canonicalize_node(child, out);
                }
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        Node::Text(t) => {
            // Trim (drops leading/trailing inter-tag whitespace) and normalize
            // volatile values. Whitespace-only nodes become empty and are
            // dropped; `<script>`/`<style>` content arrives here as raw text.
            let norm = normalize::normalize_string(t.text.trim());
            if !norm.is_empty() {
                out.push_str(&norm);
            }
        }
        // Document / doctype / comment / PI: no comparable content, but recurse
        // so the subtree below (e.g. the Document's <html> child) is visited.
        _ => {
            for child in node.children() {
                canonicalize_node(child, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_html() {
        assert!(is_html("<!DOCTYPE html>\n<html>"));
        assert!(is_html("<html><body>"));
        assert!(!is_html("OK"));
        assert!(!is_html("{\"a\":1}"));
    }

    #[test]
    fn ignores_inter_tag_whitespace_and_cache_bust() {
        let a = "<!DOCTYPE html>\n<html>\n<head>\n  <script src=\"js/x.js?1788113929\"></script>\n</head>\n</html>";
        let b = "<html><head><script src=\"js/x.js?1789999999\"></script></head></html>";
        assert_eq!(canonical_html(a), canonical_html(b));
    }

    #[test]
    fn detects_structural_difference() {
        let a = "<html><body><div id=\"feedTree\"></div></body></html>";
        let b = "<html><body><div id=\"feed-tree\"></div></body></html>";
        assert_ne!(canonical_html(a), canonical_html(b));
    }

    #[test]
    fn compares_script_content_as_text_not_tags() {
        // A `>`/`<` pair inside a <script> must not be mistaken for a tag
        // boundary (the thing the old regex got wrong), and indentation
        // (tabs vs spaces, line breaks) is collapsed so it doesn't matter.
        let a = "<html><script>if (x > 0) {\n\ty < 10;\n}</script></html>";
        let b = "<html><script>if (x > 0) { y < 10; }</script></html>";
        assert_eq!(canonical_html(a), canonical_html(b));
        // A real token change is still detected.
        let c = "<html><script>if (x > 0) { y < 99; }</script></html>";
        assert_ne!(canonical_html(a), canonical_html(c));
    }

    #[test]
    fn ignores_style_indentation() {
        let a = "<html><style>\n\tbody {\n\t\tbackground : #303030;\n\t}\n</style></html>";
        let b = "<html><style>body { background : #303030; }</style></html>";
        assert_eq!(canonical_html(a), canonical_html(b));
    }

    #[test]
    fn html_fragment_detection() {
        assert!(is_html_fragment("<p>text</p>"));
        assert!(is_html_fragment(
            "<i class=\"material-icons\">note</i>"
        ));
        // plain text / escaped text without tags is not an HTML fragment
        assert!(!is_html_fragment("&mdash; some plain text &hellip;"));
        assert!(!is_html_fragment("Grok exfiltrates user data"));
        assert!(!is_html_fragment("a < 5 and b > 3"));
        assert!(!is_html_fragment(""));
    }

    #[test]
    fn canonical_html_is_entity_form_independent() {
        // libxml's HTML4 named entities vs numeric vs literal char: all decode
        // to the same tree, so the canonical forms match.
        let a = "<p>It\u{2019}s a test &amp; more</p>";
        let b = "<p>It&#8217;s a test &amp;amp; more</p>";
        let c = "<p>It&rsquo;s a test &amp; more</p>";
        // a and c are the same tree (literal ’ and &rsquo; both decode to ’);
        // b is different text (the &amp; decodes to &amp; literally), so it
        // must NOT equal a.
        assert_eq!(canonical_html(a), canonical_html(c));
        assert_ne!(canonical_html(a), canonical_html(b));
        // whitespace between tags is dropped
        assert_eq!(canonical_html("<p>a</p> <p>b</p>"), canonical_html("<p>a</p><p>b</p>"));
    }

    #[test]
    fn normalizes_hex_in_text_and_attrs() {
        let a = "<html><script>const t = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";</script></html>";
        let b = "<html><script>const t = \"ffffffffffffffffffffffffffffffff\";</script></html>";
        assert_eq!(canonical_html(a), canonical_html(b));
    }
}
