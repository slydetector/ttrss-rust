//! M6a.2 — 1:1 port of `tt-rss/tests/FeedItemTest.php` (35 cases): RSS / Atom
//! field extraction (`FeedItem_RSS` / `FeedItem_Atom`) + the shared
//! `normalize_categories`.

use ttrss::core::feed::{FeedItem, FeedParser};
use ttrss::util::normalize_categories;

/// Parse a feed and return its collected items (asserting the feed parses).
fn items(xml: &str) -> Vec<FeedItem<'_>> {
    let mut parser = FeedParser::new(xml);
    assert!(parser.init(), "expected feed to parse successfully");
    parser.get_items().to_vec()
}

/// Build the `&[String]` argument for `normalize_categories` from literals.
fn cats(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ===== FeedItem_RSS Tests =====

#[test]
fn test_rss_get_id_from_guid() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <guid>unique-id-123</guid>
            <link>https://example.com/article</link>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "unique-id-123");
}

#[test]
fn test_rss_get_id_falls_back_to_link() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <link>https://example.com/article</link>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "https://example.com/article");
}

#[test]
fn test_rss_get_date_from_pub_date() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    // 2024-01-01 12:00:00 GMT
    assert_eq!(items[0].get_date(), Some(1704110400));
}

#[test]
fn test_rss_get_date_from_dc_date() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
    <channel>
        <item>
            <dc:date>2024-01-01T12:00:00Z</dc:date>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_date(), Some(1704110400));
}

#[test]
fn test_rss_get_date_returns_none_for_missing_date() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <title>No date</title>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_date(), None);
}

#[test]
fn test_rss_get_link_from_atom_link() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
    <channel>
        <item>
            <atom:link href="https://example.com/article" rel="alternate" />
            <link>https://example.com/old-link</link>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "https://example.com/article");
}

#[test]
fn test_rss_get_link_from_permalink_guid() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <guid isPermaLink="true">https://example.com/permalink</guid>
            <link>https://example.com/link</link>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    // Permalink guid is checked AFTER atom:link but BEFORE <link>; no
    // atom:link here, so the permalink guid is used.
    assert_eq!(items[0].get_link(), "https://example.com/permalink");
}

#[test]
fn test_rss_get_link_from_link_element() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <link>https://example.com/article</link>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "https://example.com/article");
}

#[test]
fn test_rss_get_title() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item>
            <title>  Test Article Title  </title>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_title(), "Test Article Title");
}

#[test]
fn test_rss_get_content_from_encoded() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
    <channel>
        <item>
            <content:encoded><![CDATA[<p>Full article content</p>]]></content:encoded>
            <description>Short description</description>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    let content = items[0].get_content();
    assert!(content.contains("Full article content"), "got: {content}");
}

#[test]
fn test_rss_get_content_prefers_longer() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
    <channel>
        <item>
            <content:encoded>Short</content:encoded>
            <description>This is a much longer description with more text</description>
        </item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    let content = items[0].get_content();
    assert!(content.contains("longer description"), "got: {content}");
}

// ===== FeedItem_Atom Tests =====

#[test]
fn test_atom_get_id_from_id_element() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <id>tag:example.com,2024:entry-123</id>
        <link href="https://example.com/article" />
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "tag:example.com,2024:entry-123");
}

#[test]
fn test_atom_get_id_falls_back_to_link() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <link href="https://example.com/article" />
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "https://example.com/article");
}

#[test]
fn test_atom_get_date_from_updated() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <updated>2024-01-01T12:00:00Z</updated>
        <published>2024-01-01T10:00:00Z</published>
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    // the updated time
    assert_eq!(items[0].get_date(), Some(1704110400));
}

#[test]
fn test_atom_get_date_from_published() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <published>2024-01-01T12:00:00Z</published>
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_date(), Some(1704110400));
}

#[test]
fn test_atom_get_link() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <link href="https://example.com/article" rel="alternate" />
        <link href="https://example.com/related" rel="related" />
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "https://example.com/article");
}

#[test]
fn test_atom_get_link_without_rel() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <link href="https://example.com/article" />
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "https://example.com/article");
}

#[test]
fn test_atom_get_title() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <title>  Atom Entry Title  </title>
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_title(), "Atom Entry Title");
}

#[test]
fn test_atom_get_content_from_content() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <content type="html"><![CDATA[<p>Full content</p>]]></content>
        <summary>Short summary</summary>
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    let content = items[0].get_content();
    assert!(content.contains("Full content"), "got: {content}");
}

#[test]
fn test_atom_get_content_returns_content_only() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry>
        <content type="text">Short</content>
        <summary>This is a much longer summary with more details</summary>
    </entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    // Atom get_content() returns only <content>, not <summary>
    assert_eq!(items[0].get_content(), "Short");
}

// ===== Edge Cases =====

#[test]
fn test_rss_empty_item_returns_empty_strings() {
    let items = items(r#"<?xml version="1.0"?>
<rss version="2.0">
    <channel>
        <item></item>
    </channel>
</rss>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "");
    assert_eq!(items[0].get_title(), "");
}

#[test]
fn test_atom_empty_entry_returns_empty_strings() {
    let items = items(r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
    <entry></entry>
</feed>"#);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_link(), "");
    assert_eq!(items[0].get_title(), "");
}

// ===== FeedItem_Common::normalize_categories() Tests =====

#[test]
fn test_normalize_categories_handles_single_category() {
    let result = normalize_categories(&cats(&["Technology"]));
    assert_eq!(result.len(), 1);
    assert!(result.contains(&"technology".to_string()));
}

#[test]
fn test_normalize_categories_handles_comma_separated_categories() {
    let result = normalize_categories(&cats(&["Tech,News,Programming"]));
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"tech".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(result.contains(&"programming".to_string()));
}

#[test]
fn test_normalize_categories_converts_to_lowercase() {
    let result = normalize_categories(&cats(&["TECHNOLOGY", "NeWs", "PrOgRaMmInG"]));
    assert!(result.contains(&"technology".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(result.contains(&"programming".to_string()));
    assert!(!result.contains(&"TECHNOLOGY".to_string()));
}

#[test]
fn test_normalize_categories_trims_whitespace() {
    let result = normalize_categories(&cats(&["  technology  ", " news ", "programming"]));
    assert!(result.contains(&"technology".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(!result.contains(&"  technology  ".to_string()));
}

#[test]
fn test_normalize_categories_removes_duplicates() {
    let result = normalize_categories(&cats(&["tech", "Tech", "TECH", "technology", "tech"]));
    // After lowercase normalization, 'tech' appears 4 times, 'technology' once
    assert!(result.contains(&"tech".to_string()));
    assert!(result.contains(&"technology".to_string()));
    // 'tech' should appear only once
    let tech_count = result.iter().filter(|c| c.as_str() == "tech").count();
    assert_eq!(tech_count, 1);
}

#[test]
fn test_normalize_categories_removes_empty_values() {
    let result = normalize_categories(&cats(&["tech", "", "  ", "news", ""]));
    assert_eq!(result.len(), 2);
    assert!(result.contains(&"tech".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(!result.contains(&"".to_string()));
}

#[test]
fn test_normalize_categories_handles_numeric_tags() {
    let result = normalize_categories(&cats(&["123", "456"]));
    // Numeric tags are prefixed with 't:'
    assert!(result.contains(&"t:123".to_string()));
    assert!(result.contains(&"t:456".to_string()));
    assert!(!result.contains(&"123".to_string()));
}

#[test]
fn test_normalize_categories_removes_special_characters() {
    let result = normalize_categories(&cats(&["tech,news", "tech'news", "tech\"news"]));
    // Commas split categories, quotes are removed
    assert!(result.contains(&"tech".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(result.contains(&"technews".to_string()));
}

#[test]
fn test_normalize_categories_truncates_long_categories() {
    let long_cat = "a".repeat(300);
    let result = normalize_categories(&cats(&[&long_cat]));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].chars().count(), 250);
    assert_eq!(result[0], "a".repeat(250));
}

#[test]
fn test_normalize_categories_handles_multibyte_characters() {
    let result = normalize_categories(&cats(&["技術", "ニュース", "日本"]));
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"技術".to_string()));
    assert!(result.contains(&"ニュース".to_string()));
    assert!(result.contains(&"日本".to_string()));
}

#[test]
fn test_normalize_categories_sorts_results() {
    let result = normalize_categories(&cats(&["zebra", "apple", "mango", "banana"]));
    // Results should be sorted alphabetically
    assert_eq!(result, ["apple", "banana", "mango", "zebra"]);
}

#[test]
fn test_normalize_categories_handles_empty_array() {
    let result = normalize_categories(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_normalize_categories_handles_complex_input() {
    let result = normalize_categories(&cats(&[
        "Tech,News",
        "  PROGRAMMING  ",
        "123",
        "Tech", // duplicate
        "tech'news", // special chars
        "", // empty
        &"x".repeat(300), // too long
    ]));
    // Should contain: tech, news, programming, t:123, technews, xxx... (250)
    assert!(result.contains(&"tech".to_string()));
    assert!(result.contains(&"news".to_string()));
    assert!(result.contains(&"programming".to_string()));
    assert!(result.contains(&"t:123".to_string()));
    assert!(result.contains(&"technews".to_string()));
    assert!(result.contains(&"x".repeat(250)));
    // 'tech' should appear only once despite duplicates
    let tech_count = result.iter().filter(|c| c.as_str() == "tech").count();
    assert_eq!(tech_count, 1);
}
