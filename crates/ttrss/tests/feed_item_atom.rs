//! M6a.3 — 1:1 port of `tt-rss/tests_functional/FeedItemAtomTest.php`:
//! `FeedItem_Atom`-specific field extraction (get_id, get_date, get_link,
//! get_content, get_description, get_categories, get_enclosures, get_language).
//!
//! `FeedItem_Atom` extends `FeedItem_Common`; the tests here cover the
//! Atom-specific parsing behavior not exercised by `feed_item_common.rs`
//! (shared methods) or `feed_parser.rs` (feed-level methods).

use ttrss::core::feed::{FeedItem, FeedParser};

/// Parse a feed and return its collected items (asserting the feed parses).
fn items(xml: &str) -> Vec<FeedItem<'_>> {
    let mut parser = FeedParser::new(xml);
    assert!(parser.init(), "expected feed to parse successfully");
    parser.get_items().to_vec()
}

// =========================================================================
// get_id()
// =========================================================================

#[test]
fn test_get_id_with_id_element() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>With ID</title>
    <link href="https://example.com/1" rel="alternate"/>
    <id>urn:uuid:abc123</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "urn:uuid:abc123");
}

#[test]
fn test_get_id_fallback_to_link() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No ID</title>
    <link href="https://example.com/2" rel="alternate"/>
  </entry>
</feed>"#,
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_id(), "https://example.com/2");
}

#[test]
fn test_get_id_does_not_trim_id() {
    // get_id returns the raw nodeValue, not cleaned/trimmed.
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Whitespace ID</title>
    <link href="https://example.com/3" rel="alternate"/>
    <id>  urn:uuid:whitespace  </id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_id(), "  urn:uuid:whitespace  ");
}

// =========================================================================
// get_date()
// =========================================================================

#[test]
fn test_get_date_updated() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Updated Date</title>
    <link href="https://example.com/4" rel="alternate"/>
    <id>id-4</id>
    <updated>2024-01-15T10:30:00Z</updated>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_date(), Some(1705314600));
}

#[test]
fn test_get_date_published() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Published Date</title>
    <link href="https://example.com/5" rel="alternate"/>
    <id>id-5</id>
    <published>2023-06-20T08:00:00Z</published>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_date(), Some(1687248000));
}

#[test]
fn test_get_date_prefers_updated_over_published() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Both Dates</title>
    <link href="https://example.com/6" rel="alternate"/>
    <id>id-6</id>
    <updated>2024-03-01T12:00:00Z</updated>
    <published>2023-01-01T00:00:00Z</published>
  </entry>
</feed>"#,
    );
    // Should use updated, not published.
    assert_eq!(items[0].get_date(), Some(1709294400));
}

#[test]
fn test_get_date_dc_date() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:dc="http://purl.org/dc/elements/1.1/">
  <title>Test</title>
  <entry>
    <title>DC Date</title>
    <link href="https://example.com/7" rel="alternate"/>
    <id>id-7</id>
    <dc:date>2022-11-11T00:00:00Z</dc:date>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_date(), Some(1668124800));
}

#[test]
fn test_get_date_none() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Date</title>
    <link href="https://example.com/8" rel="alternate"/>
    <id>id-8</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_date(), None);
}

// =========================================================================
// get_link()
// =========================================================================

#[test]
fn test_get_link_rel_alternate() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Link Alternate</title>
    <link href="https://example.com/9/alt" rel="alternate"/>
    <id>id-9</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/9/alt");
}

#[test]
fn test_get_link_rel_standout() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Link Standout</title>
    <link href="https://example.com/10/standout" rel="standout"/>
    <id>id-10</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/10/standout");
}

#[test]
fn test_get_link_no_rel() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Link No Rel</title>
    <link href="https://example.com/11"/>
    <id>id-11</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/11");
}

#[test]
fn test_get_link_ignores_self_rel() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Link Self Ignored</title>
    <link href="https://example.com/12/self" rel="self"/>
    <link href="https://example.com/12/alternate" rel="alternate"/>
    <id>id-12</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/12/alternate");
}

#[test]
fn test_get_link_no_link() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Link</title>
    <id>id-13</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "");
}

#[test]
fn test_get_link_xml_base() {
    // xml:base on the feed root is inherited by entries and used to resolve
    // relative link hrefs.
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.com/base/">
  <title>Test</title>
  <entry>
    <title>XML Base</title>
    <link href="relative/path" rel="alternate"/>
    <id>id-14</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/base/relative/path");
}

#[test]
fn test_get_link_trims_whitespace() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Link Whitespace</title>
    <link href="  https://example.com/15/trimmed  " rel="alternate"/>
    <id>id-15</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_link(), "https://example.com/15/trimmed");
}

// =========================================================================
// get_title()
// =========================================================================

#[test]
fn test_get_title_trims_whitespace() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>  Title With Spaces  </title>
    <link href="https://example.com/16" rel="alternate"/>
    <id>id-16</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_title(), "Title With Spaces");
}

#[test]
fn test_get_title_empty() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title></title>
    <link href="https://example.com/17" rel="alternate"/>
    <id>id-17</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_title(), "");
}

#[test]
fn test_get_title_decodes_entities() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Entity &amp;amp; &lt;test&gt;</title>
    <link href="https://example.com/18" rel="alternate"/>
    <id>id-18</id>
  </entry>
</feed>"#,
    );
    // The XML parser decodes &amp;amp; -> &amp; and &lt; -> <, &gt; -> >;
    // clean() then re-encodes & < > (htmlspecialchars).
    let title = items[0].get_title();
    assert!(title.contains("Entity"), "title = {title:?}");
    assert!(title.contains("amp"), "title = {title:?}");
}

// =========================================================================
// get_content()
// =========================================================================

#[test]
fn test_get_content_text() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Content Text</title>
    <link href="https://example.com/19" rel="alternate"/>
    <id>id-19</id>
    <content type="text">Plain text content</content>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_content(), "Plain text content");
}

#[test]
fn test_get_content_html() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Content HTML</title>
    <link href="https://example.com/20" rel="alternate"/>
    <id>id-20</id>
    <content type="html">&lt;p&gt;HTML &lt;strong&gt;content&lt;/strong&gt;&lt;/p&gt;</content>
  </entry>
</feed>"#,
    );
    let content = items[0].get_content();
    assert!(content.contains("HTML"), "content = {content:?}");
    assert!(content.contains("strong"), "content = {content:?}");
}

#[test]
fn test_get_content_xhtml() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Content XHTML</title>
    <link href="https://example.com/21" rel="alternate"/>
    <id>id-21</id>
    <content type="xhtml" xmlns="http://www.w3.org/1999/xhtml">
      <div xmlns="http://www.w3.org/1999/xhtml">
        <p>XHTML content</p>
      </div>
    </content>
  </entry>
</feed>"#,
    );
    let content = items[0].get_content();
    assert!(content.contains("XHTML content"), "content = {content:?}");
}

#[test]
fn test_get_content_no_content() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Content</title>
    <link href="https://example.com/23" rel="alternate"/>
    <id>id-23</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_content(), "");
}

#[test]
fn test_get_content_rewrites_relative_urls() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://cdn.example.com/">
  <title>Test</title>
  <entry>
    <title>Content With Base</title>
    <link href="https://example.com/22" rel="alternate"/>
    <id>id-22</id>
    <content type="text">&lt;img src="images/photo.jpg"/&gt;</content>
  </entry>
</feed>"#,
    );
    let content = items[0].get_content();
    assert!(content.contains("https://cdn.example.com/images/photo.jpg"), "content = {content:?}");
}

// =========================================================================
// get_description()
// =========================================================================

#[test]
fn test_get_description_text() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Summary Text</title>
    <link href="https://example.com/24" rel="alternate"/>
    <id>id-24</id>
    <summary>Summary description text</summary>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_description(), "Summary description text");
}

#[test]
fn test_get_description_xhtml() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Summary XHTML</title>
    <link href="https://example.com/25" rel="alternate"/>
    <id>id-25</id>
    <summary type="xhtml" xmlns="http://www.w3.org/1999/xhtml">
      <div xmlns="http://www.w3.org/1999/xhtml">
        <p>XHTML summary</p>
      </div>
    </summary>
  </entry>
</feed>"#,
    );
    let desc = items[0].get_description();
    assert!(desc.contains("XHTML summary"), "desc = {desc:?}");
}

#[test]
fn test_get_description_none() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Summary</title>
    <link href="https://example.com/26" rel="alternate"/>
    <id>id-26</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_description(), "");
}

// =========================================================================
// get_categories()
// =========================================================================

#[test]
fn test_get_categories_term() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Categories Term</title>
    <link href="https://example.com/27" rel="alternate"/>
    <id>id-27</id>
    <category term="Technology"/>
    <category term="Programming" scheme="http://example.com/scheme"/>
  </entry>
</feed>"#,
    );
    let cats = items[0].get_categories();
    assert_eq!(cats.len(), 2);
    assert!(cats.iter().any(|c| c == "technology"), "cats = {cats:?}");
    assert!(cats.iter().any(|c| c == "programming"), "cats = {cats:?}");
}

#[test]
fn test_get_categories_dc_subject() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:dc="http://purl.org/dc/elements/1.1/">
  <title>Test</title>
  <entry>
    <title>DC Subject</title>
    <link href="https://example.com/28" rel="alternate"/>
    <id>id-28</id>
    <dc:subject>Science</dc:subject>
    <dc:subject>Research</dc:subject>
  </entry>
</feed>"#,
    );
    let cats = items[0].get_categories();
    assert_eq!(cats.len(), 2);
    assert!(cats.iter().any(|c| c == "science"), "cats = {cats:?}");
    assert!(cats.iter().any(|c| c == "research"), "cats = {cats:?}");
}

#[test]
fn test_get_categories_both() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:dc="http://purl.org/dc/elements/1.1/">
  <title>Test</title>
  <entry>
    <title>Both Categories</title>
    <link href="https://example.com/29" rel="alternate"/>
    <id>id-29</id>
    <category term="News"/>
    <dc:subject>World</dc:subject>
  </entry>
</feed>"#,
    );
    let cats = items[0].get_categories();
    assert_eq!(cats.len(), 2);
    assert!(cats.iter().any(|c| c == "news"), "cats = {cats:?}");
    assert!(cats.iter().any(|c| c == "world"), "cats = {cats:?}");
}

#[test]
fn test_get_categories_none() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Categories</title>
    <link href="https://example.com/30" rel="alternate"/>
    <id>id-30</id>
  </entry>
</feed>"#,
    );
    assert!(items[0].get_categories().is_empty());
}

// =========================================================================
// get_enclosures()
// =========================================================================

#[test]
fn test_get_enclosure_rel_enclosure() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Enclosure</title>
    <link href="https://example.com/31" rel="alternate"/>
    <link href="https://example.com/podcast.mp3"
          rel="enclosure"
          type="audio/mpeg"
          length="1234567"/>
    <id>id-31</id>
  </entry>
</feed>"#,
    );
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/podcast.mp3");
    assert_eq!(encs[0].enc_type, "audio/mpeg");
    assert_eq!(encs[0].length, "1234567");
}

#[test]
fn test_get_enclosure_rewrites_relative_with_base() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://cdn.example.com/">
  <title>Test</title>
  <entry>
    <title>Enclosure With Base</title>
    <link href="https://example.com/32" rel="alternate"/>
    <link href="relative/audio.mp3"
          rel="enclosure"
          type="audio/mpeg"
          length="999"/>
    <id>id-32</id>
  </entry>
</feed>"#,
    );
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://cdn.example.com/relative/audio.mp3");
}

#[test]
fn test_get_enclosure_ignores_non_enclosure_links() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Rel Enclosure</title>
    <link href="https://example.com/33" rel="alternate"/>
    <link href="https://example.com/file.bin" type="application/octet-stream"/>
    <id>id-33</id>
  </entry>
</feed>"#,
    );
    assert!(items[0].get_enclosures().is_empty());
}

#[test]
fn test_get_enclosure_combined_with_media() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:media="http://search.yahoo.com/mrss/">
  <title>Test</title>
  <entry>
    <title>Media Enclosure</title>
    <link href="https://example.com/34" rel="alternate"/>
    <link href="https://example.com/podcast.mp3"
          rel="enclosure"
          type="audio/mpeg"
          length="555"/>
    <media:content url="https://example.com/video.mp4"
                   type="video/mp4"/>
    <id>id-34</id>
  </entry>
</feed>"#,
    );
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 2);
}

// =========================================================================
// get_language()
// =========================================================================

#[test]
fn test_get_language_entry_lang() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry xml:lang="en-US">
    <title>Entry Lang</title>
    <link href="https://example.com/35" rel="alternate"/>
    <id>id-35</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_language(), "en-US");
}

#[test]
fn test_get_language_feed_fallback() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="fr-FR">
  <title>Test</title>
  <entry>
    <title>Feed Lang Fallback</title>
    <link href="https://example.com/36" rel="alternate"/>
    <id>id-36</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_language(), "fr-FR");
}

#[test]
fn test_get_language_entry_overrides_feed() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="ja">
  <title>Test</title>
  <entry xml:lang="de-AT">
    <title>Entry Overrides Feed</title>
    <link href="https://example.com/37" rel="alternate"/>
    <id>id-37</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_language(), "de-AT");
}

#[test]
fn test_get_language_none() {
    let items = items(
        r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Lang</title>
    <link href="https://example.com/38" rel="alternate"/>
    <id>id-38</id>
  </entry>
</feed>"#,
    );
    assert_eq!(items[0].get_language(), "");
}

// =========================================================================
// multiple entries
// =========================================================================

const ATOM1_MULTIPLE_ENTRIES: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>First Entry</title>
    <link href="https://example.com/a" rel="alternate"/>
    <id>id-a</id>
    <updated>2024-01-01T00:00:00Z</updated>
  </entry>
  <entry>
    <title>Second Entry</title>
    <link href="https://example.com/b" rel="alternate"/>
    <id>id-b</id>
    <published>2023-06-15T12:00:00Z</published>
  </entry>
  <entry>
    <title>Third Entry</title>
    <link href="https://example.com/c" rel="alternate"/>
    <id>id-c</id>
  </entry>
</feed>"#;

#[test]
fn test_multiple_entries_have_independent_ids() {
    let items = items(ATOM1_MULTIPLE_ENTRIES);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].get_id(), "id-a");
    assert_eq!(items[1].get_id(), "id-b");
    assert_eq!(items[2].get_id(), "id-c");
}

#[test]
fn test_multiple_entries_have_independent_dates() {
    let items = items(ATOM1_MULTIPLE_ENTRIES);
    assert_eq!(items[0].get_date(), Some(1704067200)); // 2024-01-01T00:00:00Z
    assert_eq!(items[1].get_date(), Some(1686830400)); // 2023-06-15T12:00:00Z
    assert_eq!(items[2].get_date(), None);
}

#[test]
fn test_multiple_entries_have_independent_links() {
    let items = items(ATOM1_MULTIPLE_ENTRIES);
    assert_eq!(items[0].get_link(), "https://example.com/a");
    assert_eq!(items[1].get_link(), "https://example.com/b");
    assert_eq!(items[2].get_link(), "https://example.com/c");
}

#[test]
fn test_multiple_entries_have_independent_titles() {
    let items = items(ATOM1_MULTIPLE_ENTRIES);
    assert_eq!(items[0].get_title(), "First Entry");
    assert_eq!(items[1].get_title(), "Second Entry");
    assert_eq!(items[2].get_title(), "Third Entry");
}
