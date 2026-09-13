//! Port of `tt-rss/tests/FeedParserTest.php` — the pure (no-DB) feed-parser
//! tests, mirrored 1:1 (same inputs, same expected values).

use ttrss::core::feed::{FeedParser, FEED_ATOM, FEED_RDF, FEED_RSS};

// ===== RSS 2.0 Feed Tests (TestRss2Feed) =====

#[test]
fn test_parse_valid_rss2_feed() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>Test Feed</title>
            <link>https://example.com</link>
            <item>
                <title>Article 1</title>
                <link>https://example.com/1</link>
            </item>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_type(), FEED_RSS);
    assert_eq!(parser.get_title(), "Test Feed");
    assert_eq!(parser.get_link(), "https://example.com");
    assert_eq!(parser.get_items().len(), 1);
}

#[test]
fn test_rss_feed_trims_whitespace_in_title_and_link() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>  Whitespace Title  </title>
            <link>  https://example.com  </link>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_title(), "Whitespace Title");
    assert_eq!(parser.get_link(), "https://example.com");
}

#[test]
fn test_rss_feed_with_multiple_items() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>Test Feed</title>
            <link>https://example.com</link>
            <item><title>A1</title><link>https://example.com/1</link></item>
            <item><title>A2</title><link>https://example.com/2</link></item>
            <item><title>A3</title><link>https://example.com/3</link></item>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_items().len(), 3);
}

#[test]
fn test_rss_link_with_href_attribute() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>Test Feed</title>
            <link href="https://example.com/href-link" />
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_link(), "https://example.com/href-link");
}

#[test]
fn test_rss_link_with_node_value() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>Test Feed</title>
            <link>https://example.com/node-link</link>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_link(), "https://example.com/node-link");
}

#[test]
fn test_feed_without_title_returns_empty_string() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <link>https://example.com</link>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_title(), "");
}

#[test]
fn test_feed_without_link_returns_empty_string() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0">
        <channel>
            <title>Test Feed</title>
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_link(), "");
}

#[test]
fn test_get_links_from_rss_feed() {
    let xml = r#"<?xml version="1.0"?>
    <rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
        <channel>
            <title>Test</title>
            <atom:link rel="self" href="https://example.com/self" />
            <atom:link rel="hub" href="https://example.com/hub" />
        </channel>
    </rss>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_links("self"), vec!["https://example.com/self"]);
}

// ===== Atom Feed Tests (TestAtomFeed) =====

#[test]
fn test_parse_valid_atom_feed() {
    let xml = r#"<?xml version="1.0"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
        <title>Atom Feed</title>
        <link href="https://example.com" />
        <entry>
            <title>Entry 1</title>
            <link href="https://example.com/entry1" />
        </entry>
    </feed>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_type(), FEED_ATOM);
    assert_eq!(parser.get_title(), "Atom Feed");
    assert_eq!(parser.get_link(), "https://example.com");
    assert_eq!(parser.get_items().len(), 1);
}

#[test]
fn test_atom_feed_with_alternate_link_rel() {
    let xml = r#"<?xml version="1.0"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
        <title>Test</title>
        <link rel="alternate" href="https://example.com/alternate" />
        <link rel="self" href="https://example.com/self" />
    </feed>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_link(), "https://example.com/alternate");
}

#[test]
fn test_atom03_feed() {
    let xml = r#"<?xml version="1.0"?>
    <feed version="0.3" xmlns="http://purl.org/atom/ns#">
        <title>Atom 0.3 Feed</title>
        <link href="https://example.com" />
        <entry>
            <title>Entry</title>
            <link href="https://example.com/entry" />
        </entry>
    </feed>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_type(), FEED_ATOM);
    assert_eq!(parser.get_title(), "Atom 0.3 Feed");
    assert_eq!(parser.get_items().len(), 1);
}

#[test]
fn test_get_links_from_atom_feed() {
    let xml = r#"<?xml version="1.0"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
        <title>Test</title>
        <link rel="self" href="https://example.com/self" />
        <link rel="alternate" href="https://example.com/alternate" />
        <link rel="hub" href="https://example.com/hub" />
    </feed>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_links("self"), vec!["https://example.com/self"]);
    assert_eq!(parser.get_links("hub"), vec!["https://example.com/hub"]);
}

#[test]
fn test_get_links_without_rel_returns_all_links() {
    let xml = r#"<?xml version="1.0"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
        <title>Test</title>
        <link rel="self" href="https://example.com/self" />
        <link rel="alternate" href="https://example.com/alternate" />
    </feed>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_links("").len(), 2);
}

// ===== RDF Feed Tests (TestRdfFeed) =====

#[test]
fn test_parse_rdf_feed() {
    let xml = r#"<?xml version="1.0"?>
    <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
             xmlns="http://purl.org/rss/1.0/">
        <channel rdf:about="https://example.com">
            <title>RDF Feed</title>
            <link>https://example.com</link>
        </channel>
        <item>
            <title>Item 1</title>
            <link>https://example.com/item1</link>
        </item>
    </rdf:RDF>"#;
    let mut parser = FeedParser::new(xml);
    assert!(parser.init());
    assert_eq!(parser.get_type(), FEED_RDF);
    assert_eq!(parser.get_title(), "RDF Feed");
    assert_eq!(parser.get_link(), "https://example.com");
    assert_eq!(parser.get_items().len(), 1);
}

// ===== Error Handling Tests (TestErrorHandling) =====

#[test]
fn test_invalid_xml_returns_error() {
    let xml = r#"<?xml version="1.0"?><broken><tag></xml>"#;
    let mut parser = FeedParser::new(xml);
    assert!(!parser.init());
    assert!(!parser.error().is_empty());
}

#[test]
fn test_unknown_feed_type_returns_error() {
    let xml = r#"<?xml version="1.0"?><unknown><format /></unknown>"#;
    let mut parser = FeedParser::new(xml);
    assert!(!parser.init());
    assert_eq!(parser.get_type(), -1);
    assert!(parser.error().contains("Unknown/unsupported feed type"));
}

#[test]
fn test_empty_xml_returns_error() {
    let mut parser = FeedParser::new("");
    assert!(!parser.init());
    assert!(parser.error().contains("Empty feed data"));
}

#[test]
fn test_libxml_errors_are_collected() {
    let xml = r#"<rss><channel><title>Test</broken></channel></rss>"#;
    let mut parser = FeedParser::new(xml);
    parser.init();
    assert!(!parser.errors().is_empty());
}

// ===== Type Detection Edge Cases (TestEdgeCases) =====

#[test]
fn test_get_type_before_init_returns_unknown() {
    let xml = r#"<rss version="2.0"><channel></channel></rss>"#;
    let mut parser = FeedParser::new(xml);
    assert_eq!(parser.get_type(), FEED_RSS);
}

#[test]
fn test_get_type_caches_result() {
    let xml = r#"<rss version="2.0"><channel></channel></rss>"#;
    let mut parser = FeedParser::new(xml);
    let t1 = parser.get_type();
    let t2 = parser.get_type();
    assert_eq!(t1, t2);
}
