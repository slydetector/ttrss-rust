//! PHP `FeedItem` (classes/FeedItem.php) + the shared `FeedItem_Common`
//! helpers. A collected article is identified by its document position and
//! feed kind (RSS vs Atom); each field getter dispatches on the kind to the
//! matching extraction logic in [`super::rss`] / [`super::atom`].

use std::sync::Arc;

use roxmltree::{Document, Node, NodeId};

use super::{atom, rss, FeedKind, NS_ATOM, NS_DC, NS_MEDIA, NS_SLASH, NS_THREAD, NS_XML};

use crate::urlhelper::rewrite_relative;
use crate::util::{clean_str, is_numeric, php_str_truthy};
#[derive(Clone)]
pub struct FeedItem<'input> {
    /// The document the item lives in (shared with the parser).
    doc: Arc<Document<'input>>,
    /// The item element's position in the document.
    node_id: NodeId,
    /// The kind of article this was collected as (RSS vs Atom).
    kind: FeedKind,
}

/// PHP `FeedEnclosure` (classes/FeedEnclosure.php) — a media attachment
/// (podcast episode, video, thumbnail, …) collected by [`FeedItem::get_enclosures`].
/// The PHP `type` field is named `enc_type` here (`type` is a Rust keyword).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedEnclosure {
    pub link: String,
    pub enc_type: String,
    pub length: String,
    pub title: String,
    pub height: String,
    pub width: String,
}

impl<'input> FeedItem<'input> {
    /// Collect an item (one per `<item>`/`<entry>`).
    pub(crate) fn new(doc: Arc<Document<'input>>, node_id: NodeId, kind: FeedKind) -> Self {
        FeedItem { doc, node_id, kind }
    }

    /// The kind of article this was collected as (RSS vs Atom).
    pub fn kind(&self) -> FeedKind {
        self.kind
    }

    /// The item's element node.
    pub(crate) fn node(&self) -> Node<'_, '_> {
        self.doc
            .get_node(self.node_id)
            .expect("item node must exist in its document")
    }

    // --- field extraction (dispatches to RSS / Atom) -------------------------

    /// PHP `get_id()`: the guid/id, falling back to the link.
    pub fn get_id(&self) -> String {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_id(node),
            FeedKind::Atom => atom::get_id(node),
        }
    }

    /// PHP `get_date()`: a Unix timestamp, or `None` if no date is present or
    /// parseable (mirrors the PHP `false`).
    pub fn get_date(&self) -> Option<i64> {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_date(node),
            FeedKind::Atom => atom::get_date(node),
        }
    }

    /// PHP `get_link()`.
    pub fn get_link(&self) -> String {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_link(node),
            FeedKind::Atom => atom::get_link(node),
        }
    }

    /// PHP `get_title()`.
    pub fn get_title(&self) -> String {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_title(node),
            FeedKind::Atom => atom::get_title(node),
        }
    }

    /// PHP `get_content()`.
    pub fn get_content(&self) -> String {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_content(node),
            FeedKind::Atom => atom::get_content(node),
        }
    }

    /// PHP `get_element()`: the item's element node.
    pub fn get_element(&self) -> Node<'_, '_> {
        self.node()
    }

    /// PHP `get_categories()`: the normalized category names.
    pub fn get_categories(&self) -> Vec<String> {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_categories(node),
            FeedKind::Atom => atom::get_categories(node),
        }
    }

    /// PHP `get_enclosures()`: the collected media attachments.
    pub fn get_enclosures(&self) -> Vec<FeedEnclosure> {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_enclosures(node),
            FeedKind::Atom => atom::get_enclosures(node),
        }
    }

    /// PHP `get_language()`: the item's declared language code.
    pub fn get_language(&self) -> String {
        let node = self.node();
        let doc = self.doc.as_ref();
        match self.kind {
            FeedKind::Rss => rss::get_language(doc),
            FeedKind::Atom => atom::get_language(node),
        }
    }

    /// PHP `get_description()`: the item's description / summary text.
    pub fn get_description(&self) -> String {
        let node = self.node();
        match self.kind {
            FeedKind::Rss => rss::get_description(node),
            FeedKind::Atom => atom::get_description(node),
        }
    }

    // --- shared field extraction (identical for RSS and Atom) -----------------

    /// PHP `FeedItem_Common::get_author()`: the first `<author>` — its `<name>`,
    /// else `<email>`, else the author's text — else the `dc:creator` names
    /// joined by `", "`.
    pub fn get_author(&self) -> String {
        let node = self.node();
        if let Some(author) = first_desc_local(node, "author") {
            if let Some(name) = first_desc_local(author, "name") {
                return clean_str(&text_content(name));
            }
            if let Some(email) = first_desc_local(author, "email") {
                return clean_str(&text_content(email));
            }
            let text = text_content(author);
            if php_str_truthy(Some(&text)) {
                return clean_str(&text);
            }
        }
        all_child_ns(node, NS_DC, "creator")
            .iter()
            .map(|c| clean_str(&text_content(*c)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// PHP `FeedItem_Common::get_comments_url()`: the `<comments>` element, else
    /// an `atom:link[rel=replies]` whose `type` contains `text/html`.
    pub fn get_comments_url(&self) -> String {
        let node = self.node();
        if let Some(com) = first_child_local(node, "comments") {
            return clean_str(&text_content(com));
        }
        for link in all_child_ns(node, NS_ATOM, "link") {
            if link.attribute("rel") == Some("replies") {
                if let Some(t) = link.attribute("type") {
                    if t.contains("text/html") {
                        if let Some(href) = link.attribute("href") {
                            return clean_str(href);
                        }
                    }
                }
            }
        }
        String::new()
    }

    /// PHP `FeedItem_Common::get_comments_count()`: the first numeric of
    /// `slash:comments`, `thread:total`, or `atom:link[rel=replies]@thread:count`
    /// (in document order), else `0`.
    pub fn get_comments_count(&self) -> i32 {
        let node = self.node();
        let mut candidates: Vec<(NodeId, String)> = Vec::new();
        for c in all_child_ns(node, NS_SLASH, "comments") {
            candidates.push((c.id(), text_content(c)));
        }
        for t in all_child_ns(node, NS_THREAD, "total") {
            candidates.push((t.id(), text_content(t)));
        }
        for l in all_child_ns(node, NS_ATOM, "link") {
            if l.attribute("rel") == Some("replies") {
                if let Some(v) = l
                    .attributes()
                    .find(|a| a.name() == "count" && a.namespace() == Some(NS_THREAD))
                    .map(|a| a.value().to_string())
                {
                    candidates.push((l.id(), v));
                }
            }
        }
        if let Some((_, value)) = candidates.iter().min_by_key(|(id, _)| id.get()) {
            if is_numeric(value) {
                return clean_str(value).parse::<i32>().unwrap_or(0);
            }
        }
        0
    }
}

// --- shared tree-walk helpers (`FeedItem_Common`) ----------------------------

/// True if `n` is a `<source>` element or a descendant of one. The PHP removes
/// the item's `<source>` element in its constructor; `roxmltree` is immutable, so
/// every tree walk skips source subtrees to reproduce that.
fn under_source<'a, 'input>(n: Node<'a, 'input>) -> bool {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if c.is_element() && c.tag_name().name() == "source" {
            return true;
        }
        cur = c.parent();
    }
    false
}

/// DOM `nodeValue` equivalent: the concatenation of all descendant text nodes
/// (in document order), excluding `<source>` subtrees. `roxmltree`'s `text()`
/// only returns the first child's text, so this is needed for a faithful
/// `nodeValue`.
pub(super) fn text_content<'a, 'input>(node: Node<'a, 'input>) -> String {
    node.descendants()
        .filter(|n| n.is_text() && !under_source(*n))
        .filter_map(|n| n.text())
        .collect()
}

/// PHP `count_children($node)`: the number of descendant elements
/// (`getElementsByTagName("*")->length`), excluding the node itself and any
/// `<source>` subtrees.
pub fn count_children<'a, 'input>(node: Node<'a, 'input>) -> usize {
    node.descendants()
        .filter(|n| n.id() != node.id() && n.is_element() && !under_source(*n))
        .count()
}

/// PHP `$elem->getElementsByTagName($local)->item(0)`: the first descendant
/// element with local name `local` (any namespace), excluding `node` itself and
/// any `<source>` subtrees.
pub(super) fn first_desc_local<'a, 'input>(
    node: Node<'a, 'input>,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.descendants().find(|n| {
        n.id() != node.id() && n.is_element() && !under_source(*n) && n.tag_name().name() == local
    })
}

/// PHP `$elem->getElementsByTagName($local)`: all descendant elements with local
/// name `local` (any namespace), excluding `node` itself and `<source>` subtrees.
pub(super) fn all_desc_local<'a, 'input>(
    node: Node<'a, 'input>,
    local: &str,
) -> Vec<Node<'a, 'input>> {
    node.descendants()
        .filter(|n| {
            n.id() != node.id()
                && n.is_element()
                && !under_source(*n)
                && n.tag_name().name() == local
        })
        .collect()
}

/// PHP `$this->xpath->query("ns:local", $elem)`: all descendant elements with
/// exact namespace `ns` and local name `local`, excluding `node` itself and
/// `<source>` subtrees.
pub(super) fn all_desc_ns<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Vec<Node<'a, 'input>> {
    node.descendants()
        .filter(|n| {
            n.id() != node.id()
                && n.is_element()
                && !under_source(*n)
                && n.tag_name().namespace() == Some(ns)
                && n.tag_name().name() == local
        })
        .collect()
}

/// PHP `$this->xpath->query("local", $elem)->item(0)`: the first *direct* child
/// element with local name `local` (any namespace). A relative XPath step is the
/// child axis, not the descendant axis.
pub(super) fn first_child_local<'a, 'input>(
    node: Node<'a, 'input>,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|n| n.is_element() && !under_source(*n) && n.tag_name().name() == local)
}

/// PHP `$this->xpath->query("ns:local", $elem)->item(0)`: the first *direct*
/// child element with exact namespace `ns` and local name `local`.
pub(super) fn first_child_ns<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.children().find(|n| {
        n.is_element()
            && !under_source(*n)
            && n.tag_name().namespace() == Some(ns)
            && n.tag_name().name() == local
    })
}

/// PHP `$this->xpath->query("ns:local", $elem)`: all *direct* child elements with
/// exact namespace `ns` and local name `local`.
pub(super) fn all_child_ns<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Vec<Node<'a, 'input>> {
    node.children()
        .filter(|n| {
            n.is_element()
                && !under_source(*n)
                && n.tag_name().namespace() == Some(ns)
                && n.tag_name().name() == local
        })
        .collect()
}

/// PHP `subtree_or_text($node)`: `nodeValue` when the node has no element
/// children, otherwise its `c14n()`. `roxmltree` stores CDATA as a text node, so
/// the common feed-body case (CDATA/text) has no element children and returns the
/// text content; element-containing bodies serialize the subtree.
pub fn subtree_or_text<'a, 'input>(node: Node<'a, 'input>) -> String {
    if count_children(node) == 0 {
        return text_content(node);
    }
    c14n(node)
}

/// PHP `$node->c14n()`: the canonical XML serialization of the element subtree.
/// `roxmltree` 0.21 has no built-in node serializer, so this walks the tree
/// manually (elements + attributes + text; comments/PIs skipped).
pub(super) fn c14n<'a, 'input>(node: Node<'a, 'input>) -> String {
    let mut out = String::new();
    serialize_node(node, &mut out);
    out
}

/// Serialize `node` and its subtree into `out` (elements with attributes and
/// text nodes; comments and processing instructions are skipped).
fn serialize_node<'a, 'input>(node: Node<'a, 'input>, out: &mut String) {
    if node.is_text() {
        if let Some(text) = node.text() {
            out.push_str(text);
        }
        return;
    }
    if !node.is_element() {
        return;
    }
    let name = node.tag_name().name();
    out.push('<');
    out.push_str(name);
    for attr in node.attributes() {
        out.push(' ');
        out.push_str(attr.name());
        out.push_str("=\"");
        out.push_str(attr.value());
        out.push('"');
    }
    let children: Vec<Node<'a, 'input>> = node.children().collect();
    if children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &children {
        serialize_node(*child, out);
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// An `xml:`-prefixed attribute (e.g. `xml:base`, `xml:lang`) on `node`.
/// `roxmltree` stores the reserved `xml:` prefix as the local name with the XML
/// namespace, so this looks the local name up in the XML namespace.
pub(super) fn xml_attr<'a, 'input>(node: Node<'a, 'input>, local: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == local && a.namespace() == Some(NS_XML))
        .map(|a| a.value())
}

/// PHP `string(ancestor-or-self::*[@xml:base][1]/@xml:base)` — the nearest
/// `xml:base` on `node` or any ancestor, or `None`.
pub(super) fn xml_base<'a, 'input>(node: Node<'a, 'input>) -> Option<String> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if let Some(v) = xml_attr(n, "base") {
            return Some(v.to_string());
        }
        cur = n.parent();
    }
    None
}

/// PHP `FeedItem_Atom::rewrite_content_to_base()`: parse `$content` as a fragment
/// and rewrite every `href`/`src` attribute against `$base`. Returns `$content`
/// unchanged if `$base` is `None`, `$content` is empty, or it doesn't parse.
pub(super) fn rewrite_content_to_base(base: Option<&str>, content: &str) -> String {
    let Some(base) = base else {
        return content.to_string();
    };
    if content.is_empty() {
        return content.to_string();
    }
    let Ok(doc) = roxmltree::Document::parse(content) else {
        return content.to_string();
    };
    let mut out = content.to_string();
    for n in doc.descendants() {
        if !n.is_element() {
            continue;
        }
        for a in n.attributes() {
            if a.name() == "href" || a.name() == "src" {
                if let Some(new_val) = rewrite_relative(base, a.value()) {
                    out = out.replacen(
                        &format!("\"{}\"", a.value()),
                        &format!("\"{}\"", new_val),
                        1,
                    );
                }
            }
        }
    }
    out
}

/// `FeedItem_Common::get_enclosures()` — the shared Media-RSS part (`media:content`
/// / `media:group` / `media:thumbnail`). The RSS and Atom overrides each prepend
/// their own `<enclosure>` / `rel=enclosure` links.
pub(super) fn media_enclosures<'a, 'input>(node: Node<'a, 'input>) -> Vec<FeedEnclosure> {
    let mut encs = Vec::new();
    for content in all_child_ns(node, NS_MEDIA, "content") {
        encs.push(media_content_enclosure(content));
    }
    for group in all_child_ns(node, NS_MEDIA, "group") {
        if let Some(content) = first_child_ns(group, NS_MEDIA, "content") {
            let mut enc = media_content_enclosure(content);
            if enc.title.is_empty() {
                if let Some(desc) = first_child_ns(group, NS_MEDIA, "description") {
                    enc.title = clean_str(&text_content(desc));
                }
            }
            encs.push(enc);
        }
    }
    for thumb in all_desc_ns(node, NS_MEDIA, "thumbnail") {
        let enc = FeedEnclosure {
            enc_type: "image/generic".to_string(),
            link: clean_str(thumb.attribute("url").unwrap_or("")),
            height: clean_str(thumb.attribute("height").unwrap_or("")),
            width: clean_str(thumb.attribute("width").unwrap_or("")),
            ..Default::default()
        };
        encs.push(enc);
    }
    encs
}

/// Build a `FeedEnclosure` from a `media:content` element (type/url/length/
/// height/width + the `medium` fallback + the `media:description` child).
fn media_content_enclosure<'a, 'input>(content: Node<'a, 'input>) -> FeedEnclosure {
    let mut enc = FeedEnclosure {
        enc_type: clean_str(content.attribute("type").unwrap_or("")),
        link: clean_str(content.attribute("url").unwrap_or("")),
        length: clean_str(content.attribute("length").unwrap_or("")),
        height: clean_str(content.attribute("height").unwrap_or("")),
        width: clean_str(content.attribute("width").unwrap_or("")),
        ..Default::default()
    };
    let medium = clean_str(content.attribute("medium").unwrap_or(""));
    if enc.enc_type.is_empty() && !medium.is_empty() {
        enc.enc_type = format!("{}/generic", medium.to_lowercase());
    }
    if let Some(desc) = first_child_ns(content, NS_MEDIA, "description") {
        enc.title = clean_str(&text_content(desc));
    }
    enc
}
