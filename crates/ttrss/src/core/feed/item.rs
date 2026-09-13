//! PHP `FeedItem` (classes/FeedItem.php) — the abstract base for a collected
//! article. Holds the document position + feed kind; the concrete field
//! extraction (RSS / Atom) is layered on in M6a.2.

use std::rc::Rc;

use roxmltree::{Document, Node, NodeId};

use super::FeedKind;

#[derive(Clone)]
pub struct FeedItem<'input> {
    /// The document the item lives in (shared with the parser). Used by the
    /// M6a.2 field extraction.
    #[allow(dead_code)]
    doc: Rc<Document<'input>>,
    /// The item element's position in the document. Used by the M6a.2 field
    /// extraction.
    #[allow(dead_code)]
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

    /// The item's element node (used by the M6a.2 field extraction).
    #[allow(dead_code)]
    pub(crate) fn node(&self) -> Node<'_, '_> {
        self.doc
            .get_node(self.node_id)
            .expect("item node must exist in its document")
    }
}
