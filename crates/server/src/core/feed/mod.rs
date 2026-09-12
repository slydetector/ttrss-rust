//! Pure feed-parsing domain (the M6a port of `tt-rss/classes/FeedParser.php`
//! plus the `FeedItem_*` classes).
//!
//! This is architecture-independent: it parses feed XML into plain data
//! (feed type, link, title, and per-article fields) using [`roxmltree`] for
//! the XML tree and [`chrono`] for feed-date parsing. No database, HTTP, or
//! I/O — it mirrors the PHP, which builds a DOM and walks it with
//! `DOMXPath` / `getElementsByTagName`.
//!
//! # Namespaces & the XPath→tree-walk translation
//!
//! The PHP locates nodes with `DOMXPath` queries plus `getElementsByTagName`.
//! `roxmltree` has no XPath, so each query is translated to a local-name /
//! `(namespace, local-name)` tree walk:
//! - **Unprefixed** references (`//channel`, `//channel/item`, `title`,
//!   `description`, `getElementsByTagName("…")`) match by **local name in any
//!   namespace** — this reproduces libxml's namespace-agnostic matching that
//!   makes the PHP work with default-namespace feeds (see the spike decision
//!   in `docs/feedparser-spike.md`).
//! - **Prefixed** references (`dc:creator`, `media:content`, `atom:link`,
//!   `rssfake:item`, …) match by **exact namespace + local name**.
//!
//! # Deliberate deferrals (M6a)
//!
//! - **C14N** (`subtree_or_text` has-children branch): approximated with
//!   roxmltree's XML serialization; byte-exact canonicalization is deferred.
//! - **`xml:base` URL rewriting** in Atom `get_link`/`get_content`/
//!   `get_enclosures`, and **Atom `rewrite_content_to_base`** (relative `href`/
//!   `src` rewriting against a base URL): the base is computed but rewriting is
//!   deferred (`TODO(port): M6a`).
//! - **`<source>` removal** (`FeedItem_Common::__construct`): roxmltree is
//!   immutable, so the `<source>` element is left in the tree; the extraction
//!   queries are specific enough that it does not leak into the results.

use roxmltree::NodeId;

pub mod atom;
pub mod datetime;
pub mod enclosure;
pub mod item;
pub mod parser;
pub mod rss;
pub mod util;

pub use enclosure::FeedEnclosure;
pub use item::{normalize_categories, FeedItem};
pub use parser::FeedParser;

/// Feed type constants (mirror `FeedParser::FEED_*`).
pub const FEED_UNKNOWN: i32 = -1;
pub const FEED_RSS: i32 = 0;
pub const FEED_ATOM: i32 = 1;
pub const FEED_RDF: i32 = 2;

/// Namespaces referenced by the feed formats (mirror `FeedParser::__construct`
/// `registerNamespace` calls + `FeedItem_Atom::NS_XML`).
pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
pub const NS_ATOM03: &str = "http://purl.org/atom/ns#";
pub const NS_MEDIA: &str = "http://search.yahoo.com/mrss/";
pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_RSS10: &str = "http://purl.org/rss/1.0/";
pub const NS_SLASH: &str = "http://purl.org/rss/1.0/modules/slash/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_CONTENT: &str = "http://purl.org/rss/1.0/modules/content/";
pub const NS_THREAD: &str = "http://purl.org/syndication/thread/1.0";
pub const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

/// The kind of article an item was collected as (RSS vs Atom). Drives which
/// [`FeedItem`] implementation `FeedParser::get_items` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedKind {
    Rss,
    Atom,
}

/// A collected article: a stable [`NodeId`] (lifetime-free handle into the
/// parser's document) plus its kind. Storing the `NodeId` instead of a `Node`
/// avoids a self-reference (a `Node` borrows the document the item would live
/// in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemRef {
    pub node_id: NodeId,
    pub kind: FeedKind,
}
