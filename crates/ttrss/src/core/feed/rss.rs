//! `FeedItem_RSS` (classes/FeedItem_RSS.php) — RSS 2.0 / RSS 1.0 field
//! extraction, scoped to a single `<item>`.

use roxmltree::Node;

use super::datetime::parse_date;
use super::item::{
    FeedEnclosure, all_child_ns, all_desc_local, first_child_local, first_child_ns,
    first_desc_local, media_enclosures, subtree_or_text, text_content,
};
use super::{NS_ATOM, NS_CONTENT, NS_DC};
use crate::util::{clean_str, normalize_categories, php_trim};

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
    if let Some(date) = first_child_ns(node, NS_DC, "date") {
        return parse_date(&text_content(date));
    }
    None
}

/// PHP `FeedItem_RSS::get_link()`: an `atom:link` (no rel / `alternate` /
/// `standout`), else a permalink `<guid>`, else `<link>`.
pub(super) fn get_link<'a, 'input>(node: Node<'a, 'input>) -> String {
    for link in all_child_ns(node, NS_ATOM, "link") {
        if let Some(href) = link.attribute("href") {
            let rel = link.attribute("rel");
            if rel.is_none() || rel == Some("alternate") || rel == Some("standout") {
                return clean_str(php_trim(href));
            }
        }
    }
    if let Some(guid) = first_desc_local(node, "guid")
        && guid.attributes().next().is_some()
        && guid.attribute("isPermaLink") == Some("true")
    {
        let text = text_content(guid);
        return clean_str(php_trim(&text));
    }
    if let Some(link) = first_desc_local(node, "link") {
        let text = text_content(link);
        return clean_str(php_trim(&text));
    }
    String::new()
}

/// PHP `FeedItem_RSS::get_title()`.
pub(super) fn get_title<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(title) = first_child_local(node, "title") {
        let text = text_content(title);
        return clean_str(php_trim(&text));
    }
    if let Some(title) = first_desc_local(node, "title") {
        let text = text_content(title);
        return clean_str(php_trim(&text));
    }
    String::new()
}

/// PHP `FeedItem_RSS::get_content()`: `content:encoded` and/or `description`,
/// preferring the longer of the two.
pub(super) fn get_content<'a, 'input>(node: Node<'a, 'input>) -> String {
    let content_a = first_child_ns(node, NS_CONTENT, "encoded");
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

/// PHP `FeedItem_RSS::get_categories()`: every `<category>` (descendant) and
/// `dc:subject` (direct child) text, normalized.
pub(super) fn get_categories<'a, 'input>(node: Node<'a, 'input>) -> Vec<String> {
    let mut cats: Vec<String> = Vec::new();
    for cat in all_desc_local(node, "category") {
        cats.push(text_content(cat));
    }
    for subject in all_child_ns(node, NS_DC, "subject") {
        cats.push(text_content(subject));
    }
    normalize_categories(&cats)
}

/// PHP `FeedItem_RSS::get_enclosures()`: every `<enclosure>` element plus the
/// shared Media-RSS part (`FeedItem_Common::get_enclosures`).
pub(super) fn get_enclosures<'a, 'input>(node: Node<'a, 'input>) -> Vec<FeedEnclosure> {
    let mut encs: Vec<FeedEnclosure> = Vec::new();
    for enc in all_desc_local(node, "enclosure") {
        let e = FeedEnclosure {
            enc_type: clean_str(enc.attribute("type").unwrap_or("")),
            link: clean_str(enc.attribute("url").unwrap_or("")),
            length: clean_str(enc.attribute("length").unwrap_or("")),
            height: clean_str(enc.attribute("height").unwrap_or("")),
            width: clean_str(enc.attribute("width").unwrap_or("")),
            ..Default::default()
        };
        encs.push(e);
    }
    encs.extend(media_enclosures(node));
    encs
}

/// PHP `FeedItem_RSS::get_language()`: the document's `<language>` text, else `""`.
pub(super) fn get_language(doc: &roxmltree::Document) -> String {
    let mut found = None;
    for n in doc.descendants() {
        if n.is_element() && n.tag_name().name() == "language" {
            found = Some(n);
            break;
        }
    }
    match found {
        Some(lang) => clean_str(&text_content(lang)),
        None => String::new(),
    }
}

/// PHP `FeedItem_RSS::get_description()`: the (raw) `<description>` text, else
/// `""`. The PHP returns the raw `nodeValue` (no `clean`) here.
pub(super) fn get_description<'a, 'input>(node: Node<'a, 'input>) -> String {
    match first_desc_local(node, "description") {
        Some(desc) => text_content(desc),
        None => String::new(),
    }
}
