//! Pure feed-parsing domain — the M6a port of `tt-rss/classes/FeedParser.php`
//! and the `FeedItem_*` classes.
//!
//! This is architecture-independent: it parses feed XML into plain data (feed
//! type, title, link, and per-article fields) using [`roxmltree`] for the XML
//! tree. No database, HTTP, or I/O — it mirrors the PHP, which loads the XML
//! into a `DOMDocument` and walks it with `DOMXPath` / `getElementsByTagName`.
//!
//! # Namespaces & the XPath→tree-walk translation
//!
//! `roxmltree` has no XPath, so each PHP `DOMXPath` query is translated to a
//! (namespace, local-name) tree walk. The one subtlety (worked out in
//! `docs/feedparser-spike.md` and verified against the PHP test fixtures):
//! - **Unprefixed** references (`//channel`, `//channel/item`,
//!   `getElementsByTagName("…")`) match by **local name in any namespace** —
//!   this reproduces libxml's namespace-agnostic matching that lets the PHP
//!   work with default-namespace (unprefixed) feeds.
//! - **Prefixed** references (`dc:creator`, `media:content`, `atom:link`,
//!   `rssfake:item`, …) match by **exact namespace + local name**.
//!
//! # Status
//! - **M6a.1** (done): [`FeedParser`] — type detection + feed-level
//!   title/link + item collection (ported `tests/FeedParserTest`).
//! - **M6a.2** (done): [`FeedItem`] core field extraction (RSS / Atom
//!   id/date/link/title/content), per `tests/FeedItemTest`.
//! - **M6a.2+** (todo): enclosures / `xml:base` rewriting / `c14n()`, per
//!   `tests_functional/FeedItem*Test`.

pub mod atom;
pub mod datetime;
pub mod item;
pub mod parser;
pub mod rss;

pub use item::FeedItem;
pub use parser::FeedParser;

/// Feed type constants (mirror `FeedParser::FEED_*`).
pub const FEED_UNKNOWN: i32 = -1;
pub const FEED_RDF: i32 = 0;
pub const FEED_RSS: i32 = 1;
pub const FEED_ATOM: i32 = 2;

/// Namespaces referenced by the feed formats (mirror the
/// `FeedParser::__construct` `registerNamespace` calls + the `rssfake` alias
/// registered in `init`).
pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
pub const NS_ATOM03: &str = "http://purl.org/atom/ns#";
pub const NS_CONTENT: &str = "http://purl.org/rss/1.0/modules/content/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_RSS10: &str = "http://purl.org/rss/1.0/";
pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The kind of article an item was collected as (RSS vs Atom). Drives which
/// extraction logic a [`FeedItem`] method uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Rss,
    Atom,
}
