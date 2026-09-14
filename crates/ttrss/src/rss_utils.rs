//! Port of `tt-rss/classes/RSSUtils.php` — the feed-update engine helpers.
//!
//! **M6a.4** adds the content-encoding / gzip helpers. **M6b** adds the
//! deterministic pipeline's *pure* helpers (GUID, srcset, filter evaluation,
//! score, labels); the DB-backed pipeline (`update_rss_feed`: hash/dedup/upsert/
//! counters) and the fetch/daemon shell land later in M6b/M6c.
//!
//! # Gzip
//!
//! The common case (the server sends `Content-Encoding: gzip`) is handled by
//! the transport layer: the PHP uses `curl`'s `CURLOPT_ENCODING`, and the Rust
//! port relies on `reqwest`'s `gzip` feature. These helpers cover the fallback
//! the PHP's `RSSUtils::is_gzipped` exists for — a gzip-compressed feed body
//! that arrived *without* a transport-level decode (e.g. no `Content-Encoding`
//! header): detect it by magic bytes, then decompress.

use std::io::Read;

use regex::Regex;

use sqlx::{PgPool, Row};

use crate::core::config::Config;
use crate::core::feed::FeedParser;
use crate::feeds::decrypt_feed_pass;
use crate::urlhelper::{fetch, FetchOptions, rewrite_relative};
use crate::util::clean_str;

/// PHP `RSSUtils::is_gzipped($feed_data)` — true if the data begins with the
/// gzip magic header `\x1f\x8b\x08`.
pub fn is_gzipped(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0x1f && data[1] == 0x8b && data[2] == 0x08
}

/// PHP `gzdecode($data)` — decompress gzip bytes (via `flate2::read::GzDecoder`).
///
/// Errors if the data is not valid gzip (mirrors the PHP returning `false` for
/// a corrupt stream; callers in the M6b pipeline will treat that as a fetch
/// failure).
pub fn gzip_decode(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

// =========================================================================
// M6b — the deterministic pipeline (pure helpers)
// =========================================================================

/// A user label (`Article::_get_labels()` shape: `[feed_id, caption, fg, bg]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub feed_id: i64,
    pub caption: String,
    pub fg_color: String,
    pub bg_color: String,
}

/// A parsed `srcset` subitem (PHP `['url' => …, 'size' => …]`).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceItem {
    pub url: String,
    pub size: String,
}

/// PHP `RSSUtils::make_guid_from_title($title)` — lowercase a tag-stripped
/// title, replacing the set `[space " ' , . : ;]` with `-`.
pub fn make_guid_from_title(title: &str) -> String {
    let lower = crate::util::strip_tags(title).to_lowercase();
    lower.replace([' ', '"', '\'', ',', '.', ':', ';'], "-")
}

/// PHP `RSSUtils::calculate_article_hash($article, $pluginhost)` — the dedup
/// key for an article. Each hashed field contributes `sha1(key . ":" .
/// sha1(value))` (in `$article` order) to an accumulator; the result is
/// `sha1(implode(",", plugin_names) . tmp)`. With no plugins (this port) the
/// plugin-name prefix is empty, so it is just `sha1(tmp)`.
///
/// `hashed_fields` are the article's hashed fields as `(key, stringified
/// value)` pairs in the PHP `$article` order, with the PHP stringification
/// already applied (`content` stripped of tags; `tags`/`enclosures`
/// comma-joined). The ignored fields (`feed`, `owner_uid`, `guid`,
/// `guid_hashed`, `labels`, `force_catchup`) are omitted.
pub fn calculate_article_hash(hashed_fields: &[(String, String)]) -> String {
    let mut tmp = String::new();
    for (k, v) in hashed_fields {
        tmp.push_str(&crate::util::sha1_hex(&format!(
            "{k}:{}",
            crate::util::sha1_hex(v)
        )));
    }
    crate::util::sha1_hex(&tmp)
}

/// PHP `RSSUtils::decode_srcset($srcset)` — parse an HTML `srcset` attribute
/// into `{url, size}` items (size is a `w`/`x` descriptor or empty).
pub fn decode_srcset(srcset: &str) -> Vec<SourceItem> {
    let re = Regex::new(r"^\s*(\S+)(?:\s+(\d+w|\d+(?:\.\d+)?(?:[eE][+-]?\d+)?x))?\s*$").unwrap();
    srcset
        .split(',')
        .filter_map(|tok| {
            let caps = re.captures(tok)?;
            Some(SourceItem {
                url: caps[1].to_string(),
                size: caps
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// PHP `RSSUtils::encode_srcset($matches)` — reconstruct a `srcset` attribute
/// from parsed items.
pub fn encode_srcset(items: &[SourceItem]) -> String {
    items
        .iter()
        .map(|m| format!("{} {}", m.url.trim(), m.size.trim()))
        .collect::<Vec<_>>()
        .join(",")
}

/// PHP `RSSUtils::function_enabled($func)` — true unless the function is in
/// PHP's `disable_functions`. Rust has no such list, so this is always `true`
/// (mirrors the test expectations).
pub fn function_enabled(_func: &str) -> bool {
    true
}

/// PHP `RSSUtils::labels_contains_caption($labels, $caption)` — true if any
/// label's caption (index 1) exactly equals `$caption` (case-sensitive).
pub fn labels_contains_caption(labels: &[Label], caption: &str) -> bool {
    labels.iter().any(|l| l.caption == caption)
}

/// `RSSUtils::update_basic_info($feed_id)` — fetch the feed and, on a
/// successful parse, fill in the feed's `title` (only if empty or `[Unknown]`)
/// and `site_url` (only if it changed). The PHP first runs the
/// `HOOK_FEED_BASIC_INFO` plugin hooks to populate `$basic_info`; those are the
/// M8 plugin layer (not ported), so this goes straight to the fetch + parse
/// fallback. A no-op when the feed does not exist, the fetch returns nothing,
/// or the XML does not parse.
pub async fn update_basic_info(pool: &PgPool, cfg: &Config, feed_id: i32) {
    let row = sqlx::query(
        "SELECT feed_url, auth_login, title, site_url, auth_pass \
         FROM ttrss_feeds WHERE id = $1",
    )
    .bind(feed_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return;
    };
    let feed_url = row.get::<String, _>(0);
    let auth_login = row.get::<String, _>(1);
    let title = row.get::<String, _>(2);
    let site_url = row.get::<String, _>(3);
    let auth_pass = row.get::<String, _>(4);

    let auth_pass_plaintext = decrypt_feed_pass(&auth_pass, cfg.encryption_key.as_ref());

    let res = fetch(&FetchOptions {
        url: feed_url.clone(),
        login: if auth_login.trim().is_empty() {
            None
        } else {
            Some(auth_login)
        },
        pass: if auth_pass_plaintext.is_empty() {
            None
        } else {
            Some(auth_pass_plaintext)
        },
        timeout: Some(std::time::Duration::from_secs(cfg.feed_fetch_timeout)),
        filtering: Some(cfg.fetch_extended_url_filtering),
        ..Default::default()
    })
    .await;

    let body = res
        .body
        .as_deref()
        .map_or(String::new(), |b| String::from_utf8_lossy(b).into_owned());
    let feed_data = body.trim();
    if feed_data.is_empty() {
        return;
    }

    let mut parser = FeedParser::new(feed_data);
    if !parser.init() {
        return;
    }

    // `mb_substr(clean($rss->get_title()), 0, 199)` /
    // `mb_substr(rewrite_relative($feed_url, clean($rss->get_link())), 0, 245)`.
    let bi_title: String = clean_str(&parser.get_title()).chars().take(199).collect();
    let bi_site_url: String = rewrite_relative(&feed_url, &clean_str(&parser.get_link()))
        .unwrap_or_default()
        .chars()
        .take(245)
        .collect();

    // `!empty($basic_info['title']) && (!$feed->title || $feed->title == '[Unknown]')`.
    let new_title = if !bi_title.is_empty() && (title.is_empty() || title == "[Unknown]") {
        bi_title
    } else {
        title
    };
    // `!empty($basic_info['site_url']) && $feed->site_url != $basic_info['site_url']`.
    let new_site_url = if !bi_site_url.is_empty() && site_url != bi_site_url {
        bi_site_url
    } else {
        site_url
    };

    let _ = sqlx::query("UPDATE ttrss_feeds SET title = $1, site_url = $2 WHERE id = $3")
        .bind(&new_title)
        .bind(&new_site_url)
        .bind(feed_id)
        .execute(pool)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------------
    // is_gzipped() — gzip magic byte detection
    // (tt-rss/tests_functional/RSSUtilsTest.php)
    // ------------------------------------------------------------------------

    #[test]
    fn test_is_gzipped_valid_gzip() {
        // Gzip magic bytes: 0x1f 0x8b 0x08
        let gzip_data = [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x03];
        assert!(is_gzipped(&gzip_data));
    }

    #[test]
    fn test_is_gzipped_plaintext() {
        assert!(!is_gzipped(b"<xml>hello</xml>"));
    }

    #[test]
    fn test_is_gzipped_empty_string() {
        assert!(!is_gzipped(b""));
    }

    #[test]
    fn test_is_gzipped_short_string() {
        assert!(!is_gzipped(b"\x1f"));
        assert!(!is_gzipped(b"\x1f\x8b"));
    }

    #[test]
    fn test_is_gzipped_json_content() {
        assert!(!is_gzipped(b"{\"key\": \"value\"}"));
    }

    #[test]
    fn test_is_gzipped_xml_content() {
        assert!(!is_gzipped(
            b"<?xml version=\"1.0\"?><rss version=\"2.0\"/>"
        ));
    }

    #[test]
    fn test_is_gzipped_png_header() {
        // PNG magic bytes: 0x89 0x50 0x4e 0x47
        let png_data = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        assert!(!is_gzipped(&png_data));
    }

    #[test]
    fn test_is_gzipped_jpeg_header() {
        // JPEG magic bytes: 0xff 0xd8 0xff
        let jpeg_data = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        assert!(!is_gzipped(&jpeg_data));
    }

    // ------------------------------------------------------------------------
    // gzdecode() — gzip decode fallback
    // (tt-rss/tests_functional/FunctionsTest.php)
    // ------------------------------------------------------------------------

    #[test]
    fn test_gzdecode_basic() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let original = b"Hello, World!";
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::new(9));
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();
        let result = gzip_decode(&compressed).unwrap();
        assert_eq!(&result[..], original);
    }
    // ------------------------------------------------------------------------
    // Shared test fixtures
    // ------------------------------------------------------------------------

    fn make_label(feed_id: i64, caption: &str) -> Label {
        Label {
            feed_id,
            caption: caption.into(),
            fg_color: "#ff0000".into(),
            bg_color: "#ffffff".into(),
        }
    }

    // ------------------------------------------------------------------------
    // make_guid_from_title() — GUID derivation from an article title
    // (tt-rss/tests_functional/RSSUtilsTest.php)
    // ------------------------------------------------------------------------

    #[test]
    fn test_make_guid_from_title_simple() {
        assert_eq!(make_guid_from_title("Test Article"), "test-article");
    }

    #[test]
    fn test_make_guid_from_title_lowercase() {
        assert_eq!(
            make_guid_from_title("Already Lowercase"),
            "already-lowercase"
        );
    }

    #[test]
    fn test_make_guid_from_title_strips_html_tags() {
        // strip_tags removes the tags (not their content), then chars are replaced
        assert_eq!(make_guid_from_title("<b>bold</b> article"), "bold-article");
    }

    #[test]
    fn test_make_guid_from_title_replaces_special_chars() {
        // 5 special chars each replaced by '-'
        assert_eq!(make_guid_from_title(" ,':;"), "-----");
    }

    #[test]
    fn test_make_guid_from_title_replaces_spaces() {
        assert_eq!(make_guid_from_title("Hello World"), "hello-world");
    }

    #[test]
    fn test_make_guid_from_title_handles_unicode() {
        let result = make_guid_from_title("Café Résumé");
        assert!(result.contains("café"));
        assert!(result.contains("résumé"));
    }

    #[test]
    fn test_make_guid_from_title_empty_string() {
        assert_eq!(make_guid_from_title(""), "");
    }

    #[test]
    fn test_make_guid_from_title_only_spaces() {
        assert_eq!(make_guid_from_title("   "), "---");
    }

    #[test]
    fn test_make_guid_from_title_nested_tags() {
        assert_eq!(
            make_guid_from_title("<div><p><b>hello</b> <i>world</i></p></div>"),
            "hello-world"
        );
    }

    #[test]
    fn test_make_guid_from_title_preserves_alphanumerics_and_hyphens() {
        assert_eq!(make_guid_from_title("abc123 x y"), "abc123-x-y");
    }

    // ------------------------------------------------------------------------
    // calculate_article_hash() — article dedup hash
    // ------------------------------------------------------------------------

    #[test]
    fn test_calculate_article_hash_single_field() {
        // Locked: sha1(sha1("title:" . sha1("Hello"))).
        assert_eq!(
            calculate_article_hash(&[("title".into(), "Hello".into())]),
            "2a939a94456be4fcf1d89a346bb271ae7a98e5c4"
        );
    }

    #[test]
    fn test_calculate_article_hash_two_fields_locked() {
        // sha1(sha1("title:" . sha1("Test Article")) .
        //        sha1("content:" . sha1("<b>hi</b>"))).
        assert_eq!(
            calculate_article_hash(&[
                ("title".into(), "Test Article".into()),
                ("content".into(), "<b>hi</b>".into()),
            ]),
            "fbc28cae807004582517434454908c3baf708e19"
        );
    }

    #[test]
    fn test_calculate_article_hash_empty() {
        // sha1("") — the empty accumulator.
        assert_eq!(
            calculate_article_hash(&[]),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn test_calculate_article_hash_deterministic() {
        let f = [
            ("title".into(), "Test".into()),
            ("content".into(), "Body".into()),
        ];
        assert_eq!(calculate_article_hash(&f), calculate_article_hash(&f));
    }

    #[test]
    fn test_calculate_article_hash_field_order_matters() {
        let ab =
            calculate_article_hash(&[("title".into(), "A".into()), ("content".into(), "B".into())]);
        let ba =
            calculate_article_hash(&[("content".into(), "B".into()), ("title".into(), "A".into())]);
        assert_ne!(ab, ba);
    }

    #[test]
    fn test_calculate_article_hash_value_change_changes_hash() {
        let a =
            calculate_article_hash(&[("title".into(), "A".into()), ("content".into(), "B".into())]);
        let b =
            calculate_article_hash(&[("title".into(), "A".into()), ("content".into(), "C".into())]);
        assert_ne!(a, b);
    }

    // ------------------------------------------------------------------------
    // decode_srcset() — HTML srcset attribute parsing
    // ------------------------------------------------------------------------

    #[test]
    fn test_decode_srcset_single_entry() {
        let result = decode_srcset("image.jpg 1x");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].url, "image.jpg");
        assert_eq!(result[0].size, "1x");
    }

    #[test]
    fn test_decode_srcset_multiple_entries() {
        let result = decode_srcset("small.jpg 1x, medium.jpg 2x, large.jpg 3x");
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].url, "small.jpg");
        assert_eq!(result[0].size, "1x");
        assert_eq!(result[1].url, "medium.jpg");
        assert_eq!(result[1].size, "2x");
        assert_eq!(result[2].url, "large.jpg");
        assert_eq!(result[2].size, "3x");
    }

    #[test]
    fn test_decode_srcset_width_descriptors() {
        let result = decode_srcset("small.jpg 480w, medium.jpg 800w, large.jpg 1200w");
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].size, "480w");
        assert_eq!(result[1].size, "800w");
        assert_eq!(result[2].size, "1200w");
    }

    #[test]
    fn test_decode_srcset_leading_comma() {
        let result = decode_srcset(", image.jpg 1x");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].url, "image.jpg");
    }

    #[test]
    fn test_decode_srcset_trailing_comma() {
        let result = decode_srcset("image.jpg 1x,");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].url, "image.jpg");
    }

    #[test]
    fn test_decode_srcset_empty_string() {
        assert_eq!(decode_srcset("").len(), 0);
    }

    #[test]
    fn test_decode_srcset_no_size_descriptor() {
        let result = decode_srcset("image.jpg");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].url, "image.jpg");
        assert_eq!(result[0].size, "");
    }

    #[test]
    fn test_decode_srcset_decimal_descriptor() {
        let result = decode_srcset("image.jpg 1.5x");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].size, "1.5x");
    }

    #[test]
    fn test_decode_srcset_whitespace_handling() {
        let result = decode_srcset("  small.jpg   1x  ,   large.jpg   2x  ");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].url, "small.jpg");
        assert_eq!(result[0].size, "1x");
        assert_eq!(result[1].url, "large.jpg");
        assert_eq!(result[1].size, "2x");
    }

    // ------------------------------------------------------------------------
    // encode_srcset() — srcset reconstruction from parsed data
    // ------------------------------------------------------------------------

    #[test]
    fn test_encode_srcset_single_entry() {
        let input = [SourceItem {
            url: "image.jpg".into(),
            size: "1x".into(),
        }];
        assert_eq!(encode_srcset(&input), "image.jpg 1x");
    }

    #[test]
    fn test_encode_srcset_multiple_entries() {
        let input = [
            SourceItem {
                url: "small.jpg".into(),
                size: "1x".into(),
            },
            SourceItem {
                url: "large.jpg".into(),
                size: "2x".into(),
            },
        ];
        assert_eq!(encode_srcset(&input), "small.jpg 1x,large.jpg 2x");
    }

    #[test]
    fn test_encode_srcset_empty_array() {
        assert_eq!(encode_srcset(&[]), "");
    }

    #[test]
    fn test_encode_srcset_no_size() {
        let input = [SourceItem {
            url: "image.jpg".into(),
            size: String::new(),
        }];
        assert_eq!(encode_srcset(&input), "image.jpg ");
    }

    #[test]
    fn test_encode_srcset_roundtrip() {
        let original = "small.jpg 1x, medium.jpg 2x, large.jpg 3x";
        let decoded = decode_srcset(original);
        let encoded = encode_srcset(&decoded);
        // Round-trip should produce equivalent output (whitespace may differ).
        let decoded_again = decode_srcset(&encoded);
        assert_eq!(decoded_again.len(), 3);
        assert_eq!(decoded[0].url, decoded_again[0].url);
        assert_eq!(decoded[1].url, decoded_again[1].url);
        assert_eq!(decoded[2].url, decoded_again[2].url);
    }

    // ------------------------------------------------------------------------
    // function_enabled() — PHP disable_functions check
    // ------------------------------------------------------------------------

    #[test]
    fn test_function_enabled_common_functions() {
        // Rust has no disable_functions — every function is "enabled".
        assert!(function_enabled("strlen"));
        assert!(function_enabled("phpinfo"));
    }

    #[test]
    fn test_function_enabled_nonexistent() {
        // Non-existent functions are still "enabled" (not in any disabled list).
        assert!(function_enabled("this_function_does_not_exist_xyz"));
    }

    // ------------------------------------------------------------------------
    // labels_contains_caption() — label caption lookup
    // ------------------------------------------------------------------------

    #[test]
    fn test_labels_contains_caption_found() {
        let labels = [make_label(1, "Important"), make_label(2, "News")];
        assert!(labels_contains_caption(&labels, "Important"));
    }

    #[test]
    fn test_labels_contains_caption_not_found() {
        let labels = [make_label(1, "Important"), make_label(2, "News")];
        assert!(!labels_contains_caption(&labels, "Archive"));
    }

    #[test]
    fn test_labels_contains_caption_empty_array() {
        assert!(!labels_contains_caption(&[], "Any"));
    }

    #[test]
    fn test_labels_contains_caption_case_sensitive() {
        let labels = [make_label(1, "Important")];
        // Caption matching is case-sensitive (exact string comparison).
        assert!(!labels_contains_caption(&labels, "important"));
    }

    #[test]
    fn test_labels_contains_caption_special_chars() {
        let labels = [make_label(1, "Tag: News (2024)")];
        assert!(labels_contains_caption(&labels, "Tag: News (2024)"));
    }

    #[test]
    fn test_labels_contains_caption_empty_caption() {
        let labels = [make_label(1, "")];
        assert!(labels_contains_caption(&labels, ""));
    }
}
