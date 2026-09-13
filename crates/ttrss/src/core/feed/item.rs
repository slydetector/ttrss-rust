//! PHP `FeedItem` (classes/FeedItem.php) + the shared `FeedItem_Common`
//! helpers. A collected article is identified by its document position and
//! feed kind (RSS vs Atom); each field getter dispatches on the kind to the
//! matching extraction logic in [`super::rss`] / [`super::atom`].

use std::rc::Rc;

use roxmltree::{Document, Node, NodeId};

use super::{atom, rss, FeedKind};

#[derive(Clone)]
pub struct FeedItem<'input> {
    /// The document the item lives in (shared with the parser).
    doc: Rc<Document<'input>>,
    /// The item element's position in the document.
    node_id: NodeId,
    /// The kind of article this was collected as (RSS vs Atom).
    kind: FeedKind,
}

impl<'input> FeedItem<'input> {
    /// Collect an item (one per `<item>`/`<entry>`).
    pub(crate) fn new(doc: Rc<Document<'input>>, node_id: NodeId, kind: FeedKind) -> Self {
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
}

// --- shared tree-walk helpers (`FeedItem_Common`) ----------------------------

/// DOM `nodeValue` equivalent: the concatenation of all descendant text nodes
/// (in document order). roxmltree's `text()` only returns the first child's
/// text, so this is needed for a faithful `nodeValue`.
pub(super) fn text_content<'a, 'input>(node: Node<'a, 'input>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

/// PHP `count_children($node)`: the number of descendant elements
/// (`getElementsByTagName("*")->length`), excluding the node itself.
pub(super) fn count_children<'a, 'input>(node: Node<'a, 'input>) -> usize {
    node.descendants()
        .filter(|n| n.id() != node.id() && n.is_element())
        .count()
}

/// PHP `$elem->getElementsByTagName($local)->item(0)`: the first descendant
/// element with local name `local` (any namespace), excluding `node` itself.
pub(super) fn first_desc_local<'a, 'input>(
    node: Node<'a, 'input>,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.descendants()
        .find(|n| n.id() != node.id() && n.is_element() && n.tag_name().name() == local)
}

/// PHP `$elem->getElementsByTagName($local)`: all descendant elements with local
/// name `local` (any namespace), excluding `node` itself.
pub(super) fn all_desc_local<'a, 'input>(
    node: Node<'a, 'input>,
    local: &str,
) -> Vec<Node<'a, 'input>> {
    node.descendants()
        .filter(|n| n.id() != node.id() && n.is_element() && n.tag_name().name() == local)
        .collect()
}

/// PHP `$this->xpath->query("ns:local", $elem)->item(0)`: the first descendant
/// element with exact namespace `ns` and local name `local`, excluding `node`
/// itself.
pub(super) fn first_desc_ns<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.descendants()
        .find(|n| {
            n.id() != node.id()
                && n.is_element()
                && n.tag_name().namespace() == Some(ns)
                && n.tag_name().name() == local
        })
}

/// PHP `$this->xpath->query("ns:local", $elem)`: all descendant elements with
/// exact namespace `ns` and local name `local`, excluding `node` itself.
pub(super) fn all_desc_ns<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Vec<Node<'a, 'input>> {
    node.descendants()
        .filter(|n| {
            n.id() != node.id()
                && n.is_element()
                && n.tag_name().namespace() == Some(ns)
                && n.tag_name().name() == local
        })
        .collect()
}

/// PHP `subtree_or_text($node)`: `nodeValue` when the node has no element
/// children, otherwise its `c14n()`. roxmltree stores CDATA as a text node, so
/// the common feed-body case (CDATA/text) has no element children and returns
/// the text content. The `c14n()` branch (element-containing bodies) is
/// deferred to M6a.4 and currently also returns the text content.
pub(super) fn subtree_or_text<'a, 'input>(node: Node<'a, 'input>) -> String {
    if count_children(node) == 0 {
        return text_content(node);
    }
    // TODO(M6a.4): return `c14n()` (canonical XML) for element-containing bodies.
    text_content(node)
}
