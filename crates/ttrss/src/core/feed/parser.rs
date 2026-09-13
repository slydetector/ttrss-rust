//! Port of `tt-rss/classes/FeedParser.php` — feed type detection + feed-level
//! title/link extraction + article-item collection. Pure (no I/O).
//!
//! The PHP loads the XML into a `DOMDocument` and locates nodes with
//! `DOMXPath` / `getElementsByTagName`. `roxmltree` has no XPath, so each query
//! is translated to a (namespace, local-name) tree walk (see the module doc in
//! `mod.rs` and `docs/feedparser-spike.md`).
//!
//! `roxmltree`'s `Document<'input>` borrows from the input, so [`FeedParser`]
//! and [`FeedItem`] carry the input lifetime.

use std::rc::Rc;

use roxmltree::{Document, Node};

use super::{
    FeedItem, FeedKind, FEED_ATOM, FEED_RDF, FEED_RSS, FEED_UNKNOWN, NS_ATOM, NS_ATOM03, NS_RDF,
    NS_RSS10,
};
use crate::util::{clean_str, php_trim};

/// PHP `FeedParser` (classes/FeedParser.php). Parses feed XML (RSS 2.0 /
/// Atom 1.0 / Atom 0.3 / RSS 1.0-RDF) with [`roxmltree`] and exposes the
/// detected type, feed-level title/link, and the collected items.
pub struct FeedParser<'input> {
    /// The parsed document (shared with the collected items via `Rc`).
    doc: Option<Rc<Document<'input>>>,
    /// The first parse error (mirrors `$this->error`).
    error: Option<String>,
    /// Collected libxml-style errors (mirrors `$this->libxml_errors`).
    errors: Vec<String>,
    /// Feed title (raw; trimmed by [`Self::init`]).
    title: Option<String>,
    /// Feed link (raw; trimmed by [`Self::init`]).
    link: Option<String>,
    /// Detected feed type (`FEED_*`).
    ftype: i32,
    /// Collected items (one per article).
    items: Vec<FeedItem<'input>>,
}

impl<'input> FeedParser<'input> {
    /// PHP `FeedParser::__construct(string $data)`.
    pub fn new(data: &'input str) -> Self {
        let (doc, error, errors) = if data.is_empty() {
            (
                None,
                Some("Empty feed data provided".to_string()),
                Vec::new(),
            )
        } else {
            match Document::parse(data) {
                Ok(d) => (Some(Rc::new(d)), None, Vec::new()),
                Err(e) => {
                    let msg = e.to_string();
                    (None, Some(msg.clone()), vec![msg])
                }
            }
        };
        FeedParser {
            doc,
            error,
            errors,
            title: None,
            link: None,
            ftype: FEED_UNKNOWN,
            items: Vec::new(),
        }
    }

    /// PHP `FeedParser::init()` — detect the type and extract the feed-level
    /// title, link, and items. Returns `false` on a parse error or an
    /// unrecognized feed type.
    pub fn init(&mut self) -> bool {
        if self.error.is_some() {
            return false;
        }
        let ftype = self.get_type();
        if ftype == FEED_UNKNOWN {
            return false;
        }
        let Some(doc) = self.doc.clone() else {
            return false;
        };
        match ftype {
            FEED_ATOM => self.extract_atom(&doc),
            FEED_RSS => self.extract_rss(&doc),
            FEED_RDF => self.extract_rdf(&doc),
            _ => {}
        }
        if let Some(t) = self.title.as_mut() {
            *t = php_trim(t).to_string();
        }
        if let Some(l) = self.link.as_mut() {
            *l = php_trim(l).to_string();
        }
        true
    }

    /// PHP `FeedParser::get_type()` — detect (and cache) the feed type from the
    /// document root.
    pub fn get_type(&mut self) -> i32 {
        if self.ftype != FEED_UNKNOWN || self.error.is_some() {
            return self.ftype;
        }
        if let Some(doc) = self.doc.as_ref() {
            let root = doc.descendants().find(is_feed_root);
            if let Some(root) = root {
                let tn = root.tag_name();
                let name = tn.name().to_ascii_lowercase();
                self.ftype = match (tn.namespace(), name.as_str()) {
                    (Some(NS_RDF), "rdf") => FEED_RDF,
                    (_, "channel") => FEED_RSS,
                    (Some(NS_ATOM) | Some(NS_ATOM03), "feed") => FEED_ATOM,
                    _ => FEED_UNKNOWN,
                };
            }
        }
        if self.ftype == FEED_UNKNOWN {
            self.error
                .get_or_insert_with(|| "Unknown/unsupported feed type".to_string());
        }
        self.ftype
    }

    /// PHP `get_title()`: `clean($this->title ?? '')`.
    pub fn get_title(&self) -> String {
        clean_str(self.title.as_deref().unwrap_or(""))
    }

    /// PHP `get_link()`: `clean($this->link ?? '')`.
    pub fn get_link(&self) -> String {
        clean_str(self.link.as_deref().unwrap_or(""))
    }

    /// PHP `get_items()`: the collected items.
    pub fn get_items(&self) -> &[FeedItem<'input>] {
        &self.items
    }

    /// PHP `get_links(string $rel)`: feed links filtered by `rel` (empty = all).
    pub fn get_links(&self, rel: &str) -> Vec<String> {
        let mut rv = Vec::new();
        let Some(doc) = self.doc.as_ref() else {
            return rv;
        };
        let candidates: Vec<Node> = match self.ftype {
            FEED_ATOM => feed_links(doc, NS_ATOM),
            FEED_RSS => doc.descendants().filter(is_atom_link).collect(),
            _ => return rv,
        };
        for l in candidates {
            if rel.is_empty() || l.attribute("rel").is_some_and(|r| r == rel) {
                let href = l.attribute("href").unwrap_or("");
                rv.push(clean_str(php_trim(href)));
            }
        }
        rv
    }

    /// PHP `error()`: the first error message, or `""` on success.
    pub fn error(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    /// PHP `errors()`: the collected error messages.
    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    // --- per-type extraction (the `init()` switch arms) -----------------------

    fn extract_atom(&mut self, doc: &Rc<Document<'input>>) {
        let d = doc.as_ref();
        // title: //atom:feed/atom:title ?? //atom03:feed/atom03:title
        let title = feed_child(d, NS_ATOM, "title").or_else(|| feed_child(d, NS_ATOM03, "title"));
        if let Some(t) = title {
            self.title = Some(t.text().unwrap_or("").to_string());
        }
        // link: first <link> without rel, else rel="alternate" (atom, then atom03)
        let atom_links = feed_links(d, NS_ATOM);
        let atom03_links = feed_links(d, NS_ATOM03);
        let link = atom_links
            .iter()
            .find(|l| l.attribute("rel").is_none())
            .or_else(|| {
                atom_links
                    .iter()
                    .find(|l| l.attribute("rel") == Some("alternate"))
            })
            .or_else(|| atom03_links.iter().find(|l| l.attribute("rel").is_none()))
            .or_else(|| {
                atom03_links
                    .iter()
                    .find(|l| l.attribute("rel") == Some("alternate"))
            });
        if let Some(href) = link.and_then(|l| l.attribute("href")) {
            if !href.is_empty() {
                self.link = Some(href.to_string());
            }
        }
        // items: //atom:entry, falling back to //atom03:entry
        let mut entries: Vec<Node> = d
            .descendants()
            .filter(|n| {
                n.is_element()
                    && n.tag_name().namespace() == Some(NS_ATOM)
                    && n.tag_name().name() == "entry"
            })
            .collect();
        if entries.is_empty() {
            entries = d
                .descendants()
                .filter(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(NS_ATOM03)
                        && n.tag_name().name() == "entry"
                })
                .collect();
        }
        for e in entries {
            self.items
                .push(FeedItem::new(doc.clone(), e.id(), FeedKind::Atom));
        }
    }

    fn extract_rss(&mut self, doc: &Rc<Document<'input>>) {
        let d = doc.as_ref();
        // title: //channel/title
        if let Some(t) = child_of(d, "channel", "title") {
            self.title = Some(t.text().unwrap_or("").to_string());
        }
        // link: //channel/link — href attribute, else node value
        if let Some(link) = child_of(d, "channel", "link") {
            let value = link.text().unwrap_or("");
            match link.attribute("href") {
                Some(h) if !h.is_empty() => self.link = Some(h.to_string()),
                _ if !value.is_empty() => self.link = Some(value.to_string()),
                _ => {}
            }
        }
        // items: //channel/item
        for p in d.descendants() {
            if p.is_element() && p.tag_name().name() == "channel" {
                for c in p.children() {
                    if c.is_element() && c.tag_name().name() == "item" {
                        self.items
                            .push(FeedItem::new(doc.clone(), c.id(), FeedKind::Rss));
                    }
                }
            }
        }
    }

    fn extract_rdf(&mut self, doc: &Rc<Document<'input>>) {
        let d = doc.as_ref();
        // title: //rssfake:channel/rssfake:title
        if let Some(t) = child_of_ns(d, NS_RSS10, "channel", "title") {
            self.title = Some(t.text().unwrap_or("").to_string());
        }
        // link: //rssfake:channel/rssfake:link -> nodeValue
        if let Some(l) = child_of_ns(d, NS_RSS10, "channel", "link") {
            if let Some(text) = l.text() {
                self.link = Some(text.to_string());
            }
        }
        // items: //rssfake:item
        for n in d.descendants() {
            if n.is_element()
                && n.tag_name().namespace() == Some(NS_RSS10)
                && n.tag_name().name() == "item"
            {
                self.items
                    .push(FeedItem::new(doc.clone(), n.id(), FeedKind::Rss));
            }
        }
    }
}

// --- tree-walk helpers (the XPath→walk translation) ---------------------------

/// PHP `DOMXPath->query("(//atom03:feed|//atom:feed|//channel|//rdf:rdf|//rdf:RDF)")`
/// membership test.
fn is_feed_root(n: &Node) -> bool {
    if !n.is_element() {
        return false;
    }
    let tn = n.tag_name();
    let ns = tn.namespace();
    let name = tn.name();
    (ns == Some(NS_ATOM03) && name == "feed")
        || (ns == Some(NS_ATOM) && name == "feed")
        || name == "channel"
        || (ns == Some(NS_RDF) && (name == "rdf" || name == "RDF"))
}

/// PHP `//atom:link` membership (an element in the Atom namespace named `link`).
fn is_atom_link(n: &Node) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(NS_ATOM) && n.tag_name().name() == "link"
}

/// PHP `//{ns}:feed/{ns}:{field}` — the first `{field}` child (in `{ns}`) of a
/// `<feed>` (in `{ns}`), in document order.
fn feed_child<'a, 'input>(
    doc: &'a Document<'input>,
    feed_ns: &str,
    field: &str,
) -> Option<Node<'a, 'input>> {
    for feed in doc.descendants() {
        if feed.is_element()
            && feed.tag_name().namespace() == Some(feed_ns)
            && feed.tag_name().name() == "feed"
        {
            if let Some(c) = feed.children().find(|c| {
                c.is_element()
                    && c.tag_name().namespace() == Some(feed_ns)
                    && c.tag_name().name() == field
            }) {
                return Some(c);
            }
        }
    }
    None
}

/// PHP `//{ns}:feed/{ns}:link` — all `<link>` children (in `{ns}`) of `<feed>`
/// elements (in `{ns}`), in document order.
fn feed_links<'a, 'input>(doc: &'a Document<'input>, feed_ns: &str) -> Vec<Node<'a, 'input>> {
    let mut links = Vec::new();
    for feed in doc.descendants() {
        if feed.is_element()
            && feed.tag_name().namespace() == Some(feed_ns)
            && feed.tag_name().name() == "feed"
        {
            for c in feed.children() {
                if c.is_element()
                    && c.tag_name().namespace() == Some(feed_ns)
                    && c.tag_name().name() == "link"
                {
                    links.push(c);
                }
            }
        }
    }
    links
}

/// PHP `//{parent}/{child}` — the first element with local name `child` whose
/// parent element has local name `parent` (local names, any namespace), in
/// document order.
fn child_of<'a, 'input>(
    doc: &'a Document<'input>,
    parent: &str,
    child: &str,
) -> Option<Node<'a, 'input>> {
    for p in doc.descendants() {
        if p.is_element() && p.tag_name().name() == parent {
            if let Some(c) = p
                .children()
                .find(|c| c.is_element() && c.tag_name().name() == child)
            {
                return Some(c);
            }
        }
    }
    None
}

/// PHP `//{ns}:{parent}/{ns}:{child}` — the first `{child}` element (in `ns`)
/// whose parent element is `{parent}` (in `ns`), in document order.
fn child_of_ns<'a, 'input>(
    doc: &'a Document<'input>,
    ns: &str,
    parent: &str,
    child: &str,
) -> Option<Node<'a, 'input>> {
    for p in doc.descendants() {
        if p.is_element() && p.tag_name().namespace() == Some(ns) && p.tag_name().name() == parent {
            if let Some(c) = p.children().find(|c| {
                c.is_element()
                    && c.tag_name().namespace() == Some(ns)
                    && c.tag_name().name() == child
            }) {
                return Some(c);
            }
        }
    }
    None
}
