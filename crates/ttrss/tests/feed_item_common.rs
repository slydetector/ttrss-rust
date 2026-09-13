//! M6a.3 — 1:1 port of `tt-rss/tests_functional/FeedItemCommonTest.php`:
//! `FeedItem_Common` shared methods (get_author, get_comments_url,
//! get_comments_count, get_enclosures, normalize_categories, count_children,
//! subtree_or_text, get_element, source-element removal, get_categories).
//!
//! `FeedItem_Common` is abstract; the tests drive it through the concrete
//! `FeedItem_RSS` / `FeedItem_Atom` via `FeedParser`.

use ttrss::core::feed::{FeedItem, FeedParser};

/// Parse a feed and return its collected items (asserting the feed parses).
fn items(xml: &str) -> Vec<FeedItem<'_>> {
    let mut parser = FeedParser::new(xml);
    assert!(parser.init(), "expected feed to parse successfully");
    parser.get_items().to_vec()
}

// =========================================================================
// Fixtures — RSS 2.0
// =========================================================================

const RSS2_ITEM_AUTHOR: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Author Test</title>
      <author><name>Alice Smith</name><email>alice@example.com</email></author>
      <link>https://example.com/1</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_AUTHOR_NAME_ONLY: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Author Name Only</title>
      <author><name>Bob Jones</name></author>
      <link>https://example.com/2</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_AUTHOR_EMAIL_ONLY: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Author Email Only</title>
      <author><email>bob@example.com</email></author>
      <link>https://example.com/3</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_AUTHOR_TEXT_ONLY: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Author Text Only</title>
      <author>Charlie Brown</author>
      <link>https://example.com/4</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_DC_CREATOR: &str = r#"<rss version="2.0"
     xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Test</title>
    <item>
      <title>DC Creator</title>
      <dc:creator>Dave Author</dc:creator>
      <dc:creator>Eve Writer</dc:creator>
      <link>https://example.com/5</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_NO_AUTHOR: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>No Author</title>
      <link>https://example.com/6</link>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_COMMENTS: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Comments Test</title>
      <link>https://example.com/7</link>
      <comments>https://example.com/7/comments</comments>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_COMMENTS_COUNT: &str = r#"<rss version="2.0"
     xmlns:slash="http://purl.org/rss/1.0/modules/slash/">
  <channel>
    <title>Test</title>
    <item>
      <title>Comments Count</title>
      <link>https://example.com/8</link>
      <slash:comments>42</slash:comments>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_MEDIA_CONTENT: &str = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>Media Content</title>
      <link>https://example.com/9</link>
      <media:content url="https://example.com/video.mp4"
                     type="video/mp4"
                     length="123456"
                     height="720"
                     width="1280">
        <media:description>Video description</media:description>
      </media:content>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_MEDIA_THUMBNAIL: &str = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>Media Thumbnail</title>
      <link>https://example.com/10</link>
      <media:thumbnail url="https://example.com/thumb.jpg"
                       height="360"
                       width="640"/>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_ENCLOSURE: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Enclosure</title>
      <link>https://example.com/11</link>
      <enclosure url="https://example.com/audio.mp3"
                 type="audio/mpeg"
                 length="987654"/>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_CATEGORIES: &str = r#"<rss version="2.0"
     xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Test</title>
    <item>
      <title>Categories</title>
      <link>https://example.com/12</link>
      <category>Technology</category>
      <category>Programming, Web</category>
      <dc:subject>Design</dc:subject>
    </item>
  </channel>
</rss>"#;

const RSS2_ITEM_SOURCE_REMOVED: &str = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Source Removal</title>
      <link>https://example.com/13</link>
      <source url="https://example.com/source.rss">Source Feed</source>
    </item>
  </channel>
</rss>"#;

// =========================================================================
// Fixtures — Atom 1.0
// =========================================================================

const ATOM1_ITEM_AUTHOR: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Atom Author</title>
    <author><name>Atom Author</name><email>atom@example.com</email></author>
    <link href="https://example.com/atom/1" rel="alternate"/>
    <id>atom-id-1</id>
  </entry>
</feed>"#;

const ATOM1_ITEM_REPLIES: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Atom Replies</title>
    <link href="https://example.com/atom/2" rel="alternate"/>
    <link href="https://example.com/atom/2/replies"
          rel="replies"
          type="text/html"/>
    <id>atom-id-2</id>
  </entry>
</feed>"#;

const ATOM1_ITEM_THREADED_REPLIES: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:thread="http://purl.org/syndication/thread/1.0">
  <title>Test</title>
  <entry>
    <title>Threaded Replies</title>
    <link href="https://example.com/atom/3" rel="alternate"/>
    <link href="https://example.com/atom/3/replies"
          rel="replies"
          thread:count="7"/>
    <id>atom-id-3</id>
  </entry>
</feed>"#;

const ATOM1_ITEM_MEDIA_CONTENT: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:media="http://search.yahoo.com/mrss/">
  <title>Test</title>
  <entry>
    <title>Atom Media</title>
    <link href="https://example.com/atom/4" rel="alternate"/>
    <id>atom-id-4</id>
    <media:content url="https://example.com/atom-video.mp4"
                   type="video/mp4"
                   length="555555"/>
  </entry>
</feed>"#;

const ATOM1_ITEM_ENCLOSURE_LINK: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>Atom Enclosure Link</title>
    <link href="https://example.com/atom/5" rel="alternate"/>
    <link href="https://example.com/atom-podcast.mp3"
          rel="enclosure"
          type="audio/mpeg"
          length="1234567"/>
    <id>atom-id-5</id>
  </entry>
</feed>"#;

const ATOM1_ITEM_CATEGORIES: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:dc="http://purl.org/dc/elements/1.1/">
  <title>Test</title>
  <entry>
    <title>Atom Categories</title>
    <link href="https://example.com/atom/6" rel="alternate"/>
    <id>atom-id-6</id>
    <category term="News" scheme="http://example.com/scheme"/>
    <category term="Politics"/>
    <dc:subject>World</dc:subject>
  </entry>
</feed>"#;

const ATOM1_ITEM_NO_AUTHOR: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test</title>
  <entry>
    <title>No Author Atom</title>
    <link href="https://example.com/atom/7" rel="alternate"/>
    <id>atom-id-7</id>
  </entry>
</feed>"#;

// =========================================================================
// get_author() — RSS
// =========================================================================

#[test]
fn test_rss_get_author_with_name_and_email() {
    let items = items(RSS2_ITEM_AUTHOR);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get_author(), "Alice Smith");
}

#[test]
fn test_rss_get_author_name_only() {
    let items = items(RSS2_ITEM_AUTHOR_NAME_ONLY);
    assert_eq!(items[0].get_author(), "Bob Jones");
}

#[test]
fn test_rss_get_author_email_only() {
    let items = items(RSS2_ITEM_AUTHOR_EMAIL_ONLY);
    assert_eq!(items[0].get_author(), "bob@example.com");
}

#[test]
fn test_rss_get_author_text_only() {
    let items = items(RSS2_ITEM_AUTHOR_TEXT_ONLY);
    assert_eq!(items[0].get_author(), "Charlie Brown");
}

#[test]
fn test_rss_get_author_dc_creator_single() {
    let items = items(RSS2_ITEM_DC_CREATOR);
    // dc:creator is used when no <author> element exists.
    assert_eq!(items[0].get_author(), "Dave Author, Eve Writer");
}

#[test]
fn test_rss_get_author_none() {
    let items = items(RSS2_ITEM_NO_AUTHOR);
    assert_eq!(items[0].get_author(), "");
}

// =========================================================================
// get_author() — Atom
// =========================================================================

#[test]
fn test_atom_get_author() {
    let items = items(ATOM1_ITEM_AUTHOR);
    assert_eq!(items[0].get_author(), "Atom Author");
}

#[test]
fn test_atom_get_author_none() {
    let items = items(ATOM1_ITEM_NO_AUTHOR);
    assert_eq!(items[0].get_author(), "");
}

// =========================================================================
// get_author() — HTML entity / special character handling
// =========================================================================

#[test]
fn test_rss_get_author_with_html_entities() {
    let xml = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Entity Test</title>
      <author><name>John &amp; Jane Doe</name></author>
      <link>https://example.com/entities</link>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    assert_eq!(items[0].get_author(), "John & Jane Doe");
}

// =========================================================================
// get_author() — dc:creator fallback when <author> has no name/email
// =========================================================================

#[test]
fn test_rss_get_author_author_element_no_name_or_email() {
    let xml = r#"<rss version="2.0">
  <channel>
    <title>Test</title>
    <item>
      <title>Author Fallback</title>
      <author><someOtherElement>value</someOtherElement></author>
      <link>https://example.com/fallback</link>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    // <author> exists but has no name/email child; the concatenated text
    // ("value") is returned.
    assert_eq!(items[0].get_author(), "value");
}

// =========================================================================
// get_comments_url()
// =========================================================================

#[test]
fn test_rss_get_comments_url() {
    let items = items(RSS2_ITEM_COMMENTS);
    assert_eq!(
        items[0].get_comments_url(),
        "https://example.com/7/comments"
    );
}

#[test]
fn test_rss_get_comments_url_none() {
    let items = items(RSS2_ITEM_NO_AUTHOR);
    assert_eq!(items[0].get_comments_url(), "");
}

#[test]
fn test_atom_get_comments_url_replies() {
    let items = items(ATOM1_ITEM_REPLIES);
    assert_eq!(
        items[0].get_comments_url(),
        "https://example.com/atom/2/replies"
    );
}

// =========================================================================
// get_comments_count()
// =========================================================================

#[test]
fn test_rss_get_comments_count_slash() {
    let items = items(RSS2_ITEM_COMMENTS_COUNT);
    assert_eq!(items[0].get_comments_count(), 42);
}

#[test]
fn test_atom_get_comments_count_thread() {
    let items = items(ATOM1_ITEM_THREADED_REPLIES);
    assert_eq!(items[0].get_comments_count(), 7);
}

#[test]
fn test_rss_get_comments_count_none() {
    let items = items(RSS2_ITEM_NO_AUTHOR);
    assert_eq!(items[0].get_comments_count(), 0);
}

#[test]
fn test_atom_get_comments_count_none() {
    let items = items(ATOM1_ITEM_NO_AUTHOR);
    assert_eq!(items[0].get_comments_count(), 0);
}

// =========================================================================
// get_enclosures() — Media RSS
// =========================================================================

#[test]
fn test_rss_get_enclosures_media_content() {
    let items = items(RSS2_ITEM_MEDIA_CONTENT);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/video.mp4");
    assert_eq!(encs[0].enc_type, "video/mp4");
    assert_eq!(encs[0].length, "123456");
    assert_eq!(encs[0].height, "720");
    assert_eq!(encs[0].width, "1280");
    assert_eq!(encs[0].title, "Video description");
}

#[test]
fn test_rss_get_enclosures_media_thumbnail() {
    let items = items(RSS2_ITEM_MEDIA_THUMBNAIL);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/thumb.jpg");
    assert_eq!(encs[0].enc_type, "image/generic");
    assert_eq!(encs[0].height, "360");
    assert_eq!(encs[0].width, "640");
}

#[test]
fn test_rss_get_enclosures_rss_enclosure() {
    let items = items(RSS2_ITEM_ENCLOSURE);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/audio.mp3");
    assert_eq!(encs[0].enc_type, "audio/mpeg");
    assert_eq!(encs[0].length, "987654");
}

#[test]
fn test_rss_get_enclosures_media_and_rss_combined() {
    let xml = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>Combined Enclosures</title>
      <link>https://example.com/combined</link>
      <enclosure url="https://example.com/audio.mp3"
                 type="audio/mpeg"
                 length="1000"/>
      <media:content url="https://example.com/video.mp4"
                     type="video/mp4"/>
      <media:thumbnail url="https://example.com/thumb.jpg"/>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    let encs = items[0].get_enclosures();
    // RSS enclosure + media:content + media:thumbnail = 3
    assert_eq!(encs.len(), 3);
}

#[test]
fn test_atom_get_enclosures_media_content() {
    let items = items(ATOM1_ITEM_MEDIA_CONTENT);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/atom-video.mp4");
    assert_eq!(encs[0].enc_type, "video/mp4");
}

#[test]
fn test_atom_get_enclosures_enclosure_link() {
    let items = items(ATOM1_ITEM_ENCLOSURE_LINK);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/atom-podcast.mp3");
    assert_eq!(encs[0].enc_type, "audio/mpeg");
    assert_eq!(encs[0].length, "1234567");
}

#[test]
fn test_rss_get_enclosures_none() {
    let items = items(RSS2_ITEM_NO_AUTHOR);
    assert!(items[0].get_enclosures().is_empty());
}

#[test]
fn test_atom_get_enclosures_none() {
    let items = items(ATOM1_ITEM_NO_AUTHOR);
    assert!(items[0].get_enclosures().is_empty());
}

// =========================================================================
// get_enclosures() — media:group
// =========================================================================

#[test]
fn test_rss_get_enclosures_media_group() {
    let xml = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>Media Group</title>
      <link>https://example.com/group</link>
      <media:group>
        <media:content url="https://example.com/highres.jpg"
                       type="image/jpeg"
                       height="1080"
                       width="1920"/>
        <media:description>High resolution image</media:description>
      </media:group>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].link, "https://example.com/highres.jpg");
    assert_eq!(encs[0].enc_type, "image/jpeg");
    assert_eq!(encs[0].height, "1080");
    assert_eq!(encs[0].width, "1920");
    assert_eq!(encs[0].title, "High resolution image");
}

// =========================================================================
// get_enclosures() — medium attribute fallback
// =========================================================================

#[test]
fn test_rss_get_enclosures_medium_attribute() {
    let xml = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>Medium Attribute</title>
      <link>https://example.com/medium</link>
      <media:content medium="video" url="https://example.com/video.webm"/>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].enc_type, "video/generic");
}

// =========================================================================
// get_enclosures() — media:content no type (medium fallback)
// =========================================================================

#[test]
fn test_get_enclosures_media_content_no_type() {
    let xml = r#"<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Test</title>
    <item>
      <title>No Type</title>
      <link>https://example.com/no-type</link>
      <media:content url="https://example.com/file.bin" medium="application"/>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    let encs = items[0].get_enclosures();
    assert_eq!(encs.len(), 1);
    assert_eq!(encs[0].enc_type, "application/generic");
}

// =========================================================================
// normalize_categories() — static method
// =========================================================================

fn cats(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_normalize_categories_simple() {
    let result = ttrss::util::normalize_categories(&cats(&["Technology", "Programming"]));
    assert_eq!(
        result,
        vec!["programming".to_string(), "technology".to_string()]
    );
}

#[test]
fn test_normalize_categories_csv_split() {
    let result = ttrss::util::normalize_categories(&cats(&["Tech, Programming"]));
    assert_eq!(result, vec!["programming".to_string(), "tech".to_string()]);
}

#[test]
fn test_normalize_categories_csv_multiple() {
    let result = ttrss::util::normalize_categories(&cats(&["A, B", "C, D"]));
    assert_eq!(
        result,
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string()
        ]
    );
}

#[test]
fn test_normalize_categories_numeric_prefix() {
    let result = ttrss::util::normalize_categories(&cats(&["123", "456"]));
    assert_eq!(result, vec!["t:123".to_string(), "t:456".to_string()]);
}

#[test]
fn test_normalize_categories_lowercase() {
    let result = ttrss::util::normalize_categories(&cats(&["Technology", "PROGRAMMING"]));
    assert_eq!(
        result,
        vec!["programming".to_string(), "technology".to_string()]
    );
}

#[test]
fn test_normalize_categories_strip_quotes() {
    let result = ttrss::util::normalize_categories(&cats(&["'Tech'", "\"Web\""]));
    assert_eq!(result, vec!["tech".to_string(), "web".to_string()]);
}

#[test]
fn test_normalize_categories_trim_whitespace() {
    let result = ttrss::util::normalize_categories(&cats(&["  Technology  "]));
    assert_eq!(result, vec!["technology".to_string()]);
}

#[test]
fn test_normalize_categories_truncate_long() {
    let long = "a".repeat(300);
    let result = ttrss::util::normalize_categories(&[long]);
    assert_eq!(result[0].len(), 250);
}

#[test]
fn test_normalize_categories_remove_empty() {
    let result = ttrss::util::normalize_categories(&cats(&["", "  ", "Valid"]));
    assert_eq!(result, vec!["valid".to_string()]);
}

#[test]
fn test_normalize_categories_empty_input() {
    let result = ttrss::util::normalize_categories(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_normalize_categories_dedup() {
    let result = ttrss::util::normalize_categories(&cats(&["Tech", "tech", "TECH"]));
    assert_eq!(result, vec!["tech".to_string()]);
}

#[test]
fn test_normalize_categories_mixed() {
    let result = ttrss::util::normalize_categories(&cats(&["News, Politics", "world", "123"]));
    assert_eq!(
        result,
        vec![
            "news".to_string(),
            "politics".to_string(),
            "t:123".to_string(),
            "world".to_string()
        ]
    );
}

// =========================================================================
// count_children()
// =========================================================================

#[test]
fn test_count_children_empty() {
    let doc = roxmltree::Document::parse(r#"<root/>"#).unwrap();
    let root = doc.root_element();
    assert_eq!(ttrss::core::feed::item::count_children(root), 0);
}

#[test]
fn test_count_children_with_siblings() {
    let doc =
        roxmltree::Document::parse(r#"<parent><child1/><child2/><child3/></parent>"#).unwrap();
    let parent = doc.root_element();
    assert_eq!(ttrss::core::feed::item::count_children(parent), 3);
}

// =========================================================================
// subtree_or_text()
// =========================================================================

#[test]
fn test_subtree_or_text_leaf_node() {
    let doc = roxmltree::Document::parse(r#"<title>Simple Title</title>"#).unwrap();
    let node = doc.root_element();
    assert_eq!(
        ttrss::core::feed::item::subtree_or_text(node),
        "Simple Title"
    );
}

#[test]
fn test_subtree_or_text_element_with_children() {
    let doc = roxmltree::Document::parse(r#"<content>Hello <b>world</b></content>"#).unwrap();
    let node = doc.root_element();
    let result = ttrss::core::feed::item::subtree_or_text(node);
    assert_ne!(result, "Hello world");
    // Should return c14n serialized XML, not plain text.
    assert!(result.contains("<b>"), "result = {result:?}");
}

// =========================================================================
// get_element()
// =========================================================================

#[test]
fn test_get_element_returns_element() {
    let items = items(RSS2_ITEM_NO_AUTHOR);
    let elem = items[0].get_element();
    assert!(elem.is_element());
    assert_eq!(elem.tag_name().name(), "item");
}

// =========================================================================
// Source element removal in constructor
// =========================================================================

#[test]
fn test_constructor_removes_source_element() {
    // NOTE(port): the PHP removes the `<source>` element from the DOM in the
    // constructor, so `get_element()` has no `<source>` descendant. `roxmltree`
    // is immutable, so the Rust port skips `<source>` subtrees in every tree
    // walk instead of removing them; the observable behavior (the source is not
    // counted / surfaced) is reproduced by `count_children` skipping it. The
    // item here has title + link + source, so the source-skipping child count
    // is 2, not 3.
    let items = items(RSS2_ITEM_SOURCE_REMOVED);
    let elem = items[0].get_element();
    assert_eq!(ttrss::core::feed::item::count_children(elem), 2);
}

// =========================================================================
// get_categories() — via normalize_categories (common logic)
// =========================================================================

#[test]
fn test_rss_get_categories() {
    let items = items(RSS2_ITEM_CATEGORIES);
    let cats = items[0].get_categories();
    assert_eq!(
        cats,
        vec![
            "design".to_string(),
            "programming".to_string(),
            "technology".to_string(),
            "web".to_string()
        ]
    );
}

#[test]
fn test_atom_get_categories() {
    let items = items(ATOM1_ITEM_CATEGORIES);
    let cats = items[0].get_categories();
    assert_eq!(
        cats,
        vec![
            "news".to_string(),
            "politics".to_string(),
            "world".to_string()
        ]
    );
}

// =========================================================================
// get_comments_count() — non-numeric value
// =========================================================================

#[test]
fn test_get_comments_count_non_numeric() {
    let xml = r#"<rss version="2.0"
     xmlns:slash="http://purl.org/rss/1.0/modules/slash/">
  <channel>
    <title>Test</title>
    <item>
      <title>Non-numeric Comments</title>
      <link>https://example.com/nonnumeric</link>
      <slash:comments>many</slash:comments>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    assert_eq!(items[0].get_comments_count(), 0);
}

// =========================================================================
// get_comments_url() — atom:link rel=replies in RSS
// =========================================================================

#[test]
fn test_rss_get_comments_url_atom_replies() {
    let xml = r#"<rss version="2.0"
     xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>Test</title>
    <item>
      <title>Atom Replies in RSS</title>
      <link>https://example.com/atom-replies</link>
      <atom:link href="https://example.com/atom-replies/comments"
                 rel="replies"
                 type="text/html"/>
    </item>
  </channel>
</rss>"#;
    let items = items(xml);
    assert_eq!(
        items[0].get_comments_url(),
        "https://example.com/atom-replies/comments"
    );
}
