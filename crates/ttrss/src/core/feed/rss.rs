//! `FeedItem_RSS` (classes/FeedItem_RSS.php) — RSS 2.0 / RSS 1.0 field
//! extraction, scoped to a single `<item>`.

use roxmltree::Node;

use super::datetime::parse_date;
use super::item::{
    all_desc_ns, first_desc_local, first_desc_ns, subtree_or_text, text_content,
};
use super::{NS_ATOM, NS_CONTENT, NS_DC};
use crate::util::{clean_str, php_trim};

/// PHP `FeedItem_RSS::get_id()`: the guid, else the (cleaned) link.
pub(super) fn get_id<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(guid) = first_desc_local(node, "guid") {
        return clean_str(&text_content(guid));
    }
    clean_str(&get_link(node))
}

/// PHP `FeedItem_RSS::get_date()`: `pubDate` else `dc:date`, else `None`.
pub(super) fn get_date<'a, 'input>(node: Node<'a, 'input>) -> Option<i64> {
    if let Some(pub_date) = first_desc_local(node, "pubDate") {
        return parse_date(&text_content(pub_date));
    }
    if let Some(date) = first_desc_ns(node, NS_DC, "date") {
        return parse_date(&text_content(date));
    }
    None
}

/// PHP `FeedItem_RSS::get_link()`: an `atom:link` (no rel / `alternate` /
/// `standout`), else a permalink `<guid>`, else `<link>`.
pub(super) fn get_link<'a, 'input>(node: Node<'a, 'input>) -> String {
    for link in all_desc_ns(node, NS_ATOM, "link") {
        if let Some(href) = link.attribute("href") {
            let rel = link.attribute("rel");
            if rel.is_none() || rel == Some("alternate") || rel == Some("standout") {
                return clean_str(php_trim(href));
            }
        }
    }
    if let Some(guid) = first_desc_local(node, "guid") {
        if guid.attributes().next().is_some() && guid.attribute("isPermaLink") == Some("true") {
            let text = text_content(guid);
            return clean_str(php_trim(&text));
        }
    }
    if let Some(link) = first_desc_local(node, "link") {
        let text = text_content(link);
        return clean_str(php_trim(&text));
    }
    String::new()
}

/// PHP `FeedItem_RSS::get_title()`.
pub(super) fn get_title<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(title) = first_desc_local(node, "title") {
        let text = text_content(title);
        return clean_str(php_trim(&text));
    }
    String::new()
}

/// PHP `FeedItem_RSS::get_content()`: `content:encoded` and/or `description`,
/// preferring the longer of the two.
pub(super) fn get_content<'a, 'input>(node: Node<'a, 'input>) -> String {
    let content_a = first_desc_ns(node, NS_CONTENT, "encoded");
    let content_b = first_desc_local(node, "description");
    match (content_a, content_b) {
        (Some(a), Some(b)) => {
            let ra = subtree_or_text(a);
            let rb = subtree_or_text(b);
            if ra.chars().count() > rb.chars().count() {
                ra
            } else {
                rb
            }
        }
        (Some(a), None) => subtree_or_text(a),
        (None, Some(b)) => subtree_or_text(b),
        (None, None) => String::new(),
    }
}
