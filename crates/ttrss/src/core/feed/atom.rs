//! `FeedItem_Atom` (classes/FeedItem_Atom.php) — Atom 1.0 / Atom 0.3 field
//! extraction, scoped to a single `<entry>`.

use roxmltree::Node;

use super::datetime::parse_date;
use super::item::{
    all_child_ns, all_desc_local, c14n, first_child_ns, first_desc_local, media_enclosures,
    rewrite_content_to_base, subtree_or_text, text_content, xml_attr, xml_base, FeedEnclosure,
};
use super::NS_DC;
use crate::urlhelper::rewrite_relative;
use crate::util::{clean_str, normalize_categories, php_trim};

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
    if let Some(date) = first_child_ns(node, NS_DC, "date") {
        return parse_date(&text_content(date));
    }
    None
}

/// PHP `FeedItem_Atom::get_link()`: the first `<link>` with href and (no rel /
/// `alternate` / `standout`).
pub(super) fn get_link<'a, 'input>(node: Node<'a, 'input>) -> String {
    for link in all_desc_local(node, "link") {
        let Some(href) = link.attribute("href") else {
            continue;
        };
        let rel = link.attribute("rel");
        if rel.is_none() || rel == Some("alternate") || rel == Some("standout") {
            let base = xml_base(link);
            let val = clean_str(php_trim(href));
            return match base {
                Some(b) => rewrite_relative(&b, &val).unwrap_or(val),
                None => val,
            };
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
    atom_body(node, "content")
}

/// PHP `FeedItem_Atom::get_description()`: the `<summary>`, with `xml:base`
/// rewriting and `type="xhtml"` handling (the PHP notes this duplicates
/// `get_content`).
pub(super) fn get_description<'a, 'input>(node: Node<'a, 'input>) -> String {
    atom_body(node, "summary")
}

/// Shared body extraction for `get_content` (`content`) and `get_description`
/// (`summary`): the `xml:base` rewriting + `type="xhtml"` handling.
fn atom_body<'a, 'input>(node: Node<'a, 'input>, elem: &str) -> String {
    let Some(content) = first_desc_local(node, elem) else {
        return String::new();
    };
    let base = xml_base(content);
    if content.attribute("type") == Some("xhtml") {
        for child in content.children() {
            if child.children().next().is_some() {
                return rewrite_content_to_base(base.as_deref(), &c14n(child));
            }
        }
    }
    rewrite_content_to_base(base.as_deref(), &subtree_or_text(content))
}

/// PHP `FeedItem_Atom::get_categories()`: every `<category>` `term` attribute
/// (descendant) and `dc:subject` (direct child) text, normalized.
pub(super) fn get_categories<'a, 'input>(node: Node<'a, 'input>) -> Vec<String> {
    let mut cats: Vec<String> = Vec::new();
    for cat in all_desc_local(node, "category") {
        if let Some(term) = cat.attribute("term") {
            cats.push(term.to_string());
        }
    }
    for subject in all_child_ns(node, NS_DC, "subject") {
        cats.push(text_content(subject));
    }
    normalize_categories(&cats)
}

/// PHP `FeedItem_Atom::get_enclosures()`: every `<link rel="enclosure">` (with
/// `xml:base` rewriting) plus the shared Media-RSS part.
pub(super) fn get_enclosures<'a, 'input>(node: Node<'a, 'input>) -> Vec<FeedEnclosure> {
    let mut encs: Vec<FeedEnclosure> = Vec::new();
    for link in all_desc_local(node, "link") {
        let Some(href) = link.attribute("href") else {
            continue;
        };
        if link.attribute("rel") != Some("enclosure") {
            continue;
        }
        let base = xml_base(link);
        let mut link_val = clean_str(href);
        if let Some(b) = &base {
            if let Some(rewritten) = rewrite_relative(b, &link_val) {
                link_val = rewritten;
            }
        }
        let enc = FeedEnclosure {
            enc_type: clean_str(link.attribute("type").unwrap_or("")),
            length: clean_str(link.attribute("length").unwrap_or("")),
            link: link_val,
            ..Default::default()
        };
        encs.push(enc);
    }
    encs.extend(media_enclosures(node));
    encs
}

/// PHP `FeedItem_Atom::get_language()`: the entry's `xml:lang`, else the feed's
/// `xml:lang`, else `""`.
pub(super) fn get_language<'a, 'input>(node: Node<'a, 'input>) -> String {
    if let Some(lang) = xml_attr(node, "lang") {
        if !lang.is_empty() {
            return clean_str(lang);
        }
    }
    if let Some(feed) = node.parent() {
        if feed.is_element() {
            return clean_str(xml_attr(feed, "lang").unwrap_or(""));
        }
    }
    String::new()
}
