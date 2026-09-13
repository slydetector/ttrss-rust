//! `FeedItem_Atom` (classes/FeedItem_Atom.php) — Atom 1.0 / Atom 0.3 field
//! extraction, scoped to a single `<entry>`.

use roxmltree::Node;

use super::datetime::parse_date;
use super::item::{all_desc_local, first_desc_local, first_desc_ns, subtree_or_text, text_content};
use super::NS_DC;
use crate::util::{clean_str, php_trim};

/// PHP `FeedItem_Atom::get_id()`: the `<id>`, else the (cleaned) link.
pub(super) fn get_id<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(id) = first_desc_local(node, "id") {
        // NOTE: the PHP returns the raw nodeValue here (no `clean()`), unlike
        // the RSS guid path.
        return text_content(id);
    }
    clean_str(&get_link(node))
}

/// PHP `FeedItem_Atom::get_date()`: `updated` else `published` else `dc:date`,
/// else `None`.
pub(super) fn get_date<'a, 'input>(node: Node<'a, 'input>) -> Option<i64> {
    if let Some(updated) = first_desc_local(node, "updated") {
        return parse_date(&text_content(updated));
    }
    if let Some(published) = first_desc_local(node, "published") {
        return parse_date(&text_content(published));
    }
    if let Some(date) = first_desc_ns(node, NS_DC, "date") {
        return parse_date(&text_content(date));
    }
    None
}

/// PHP `FeedItem_Atom::get_link()`: the first `<link>` with href and (no rel /
/// `alternate` / `standout`).
pub(super) fn get_link<'a, 'input>(node: Node<'a, 'input>) -> String {
    for link in all_desc_local(node, "link") {
        if let Some(href) = link.attribute("href") {
            let rel = link.attribute("rel");
            if rel.is_none() || rel == Some("alternate") || rel == Some("standout") {
                // xml:base rewriting (`UrlHelper::rewrite_relative`) is
                // deferred to M6a.4.
                return clean_str(php_trim(href));
            }
        }
    }
    String::new()
}

/// PHP `FeedItem_Atom::get_title()`.
pub(super) fn get_title<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(title) = first_desc_local(node, "title") {
        let text = text_content(title);
        return clean_str(php_trim(&text));
    }
    String::new()
}

/// PHP `FeedItem_Atom::get_content()`: the `<content>` (not `<summary>`).
pub(super) fn get_content<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(content) = first_desc_local(node, "content") {
        // `xml:base` rewriting + `type="xhtml"` handling are deferred to M6a.4.
        return subtree_or_text(content);
    }
    String::new()
}
