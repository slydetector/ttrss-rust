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
use std::sync::Arc;

use html5ever::{LocalName, QualName};
use regex::Regex;
use scraper::Html;
use serde_json::Value;

use sqlx::{PgPool, Row};

/// html5ever stores HTML attribute names namespaceless — lookups must match.
fn attr_name(name: &str) -> QualName {
    QualName::new(
        None,
        html5ever::Namespace::from(""),
        LocalName::from(name.to_string()),
    )
}

use crate::core::config::Config;
use crate::core::feed::{FeedEnclosure, FeedParser};
use crate::diskcache::DiskCache;
use crate::feeds::decrypt_feed_pass;
use crate::pluginhost::PluginHost;
use crate::urlhelper::{FetchOptions, fetch, rewrite_relative};
use crate::util::{clean_str, sha1_hex};

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

/// PHP `json_encode(["ver" => 2, "uid" => $owner_uid, "hash" => 'SHA1:' . sha1($entry_guid)])`
/// — the `guid_hashed` string stored in `ttrss_entries.guid`.
///
/// PHP `json_encode` preserves insertion order (`ver`, `uid`, `hash`). We emit
/// that exact byte order (rather than serde_json's sorted `Map` order, which
/// would be `hash`, `uid`, `ver`) so the string `guid IN (…)` dedup matches rows
/// created by the PHP daemon — otherwise every re-fetched PHP article is duplicated.
pub fn make_guid_hashed(owner_uid: i32, entry_guid: &str) -> String {
    format!(
        "{{\"ver\":2,\"uid\":{owner_uid},\"hash\":\"SHA1:{}\"}}",
        crate::util::sha1_hex(entry_guid)
    )
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
        tmp.push_str(&crate::util::sha1_hex(format!(
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

// =========================================================================
// Media disk caching (the per-feed `cache_images` feature)
// =========================================================================

/// The media URLs of `$html` — PHP's XPath
/// `(//img[@src]|//source[@src|@srcset]|//video[@poster|@src])`: the
/// `src`/`poster` attributes (non-`data:`) and every `srcset` item,
/// absolutized against `$site_url` (`rewrite_relative`; unresolvable URLs
/// are dropped).
pub fn media_urls(html: &str, site_url: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let html_doc = Html::parse_document(html);
    let doc = html_doc.tree.root();
    for node in doc.descendants() {
        let Some(el) = node.value().as_element() else {
            continue;
        };
        let name = el.name.local.to_string();
        if !matches!(name.as_str(), "img" | "source" | "video") {
            continue;
        }
        for attr in ["src", "poster"] {
            let Some(raw) = el.attrs.get(&attr_name(attr)).map(|t| t.to_string()) else {
                continue;
            };
            if raw.starts_with("data:") {
                continue;
            }
            if let Some(url) = rewrite_relative(site_url, &raw) {
                urls.push(url);
            }
        }
        if let Some(srcset) = el.attrs.get(&attr_name("srcset")).map(|t| t.to_string()) {
            for item in decode_srcset(&srcset) {
                if let Some(url) = rewrite_relative(site_url, &item.url) {
                    urls.push(url);
                }
            }
        }
    }
    urls
}

/// PHP `RSSUtils::cache_media_url($cache, $url, $site_url)` — absolutize
/// `$url`, and download it (referrer = the URL itself, `MAX_CACHE_FILE_SIZE`
/// cap) to the cache as `sha1($abs)` if absent.
pub async fn cache_media_url(cache: &DiskCache, raw_url: &str, site_url: &str, max_size: u64) {
    let Some(abs) = rewrite_relative(site_url, raw_url) else {
        return;
    };
    let local = sha1_hex(&abs);
    let _ = cache
        .download(&abs, &local, max_size, Some(&abs), false)
        .await;
}

/// PHP `RSSUtils::cache_media($html, $site_url)` — download every media URL
/// in the article content to the shared `images` cache.
pub async fn cache_media(cache: &DiskCache, html: &str, site_url: &str, max_size: u64) {
    if html.is_empty() {
        return;
    }
    for url in media_urls(html, site_url) {
        cache_media_url(cache, &url, site_url, max_size).await;
    }
}

/// PHP `RSSUtils::cache_enclosures($enclosures, $site_url)` — download the
/// image/audio/video enclosure media to the shared `images` cache (the
/// type gate is PHP's `preg_match("/(image|audio|video)/", $enc->type)`).
pub async fn cache_enclosures(
    cache: &DiskCache,
    enclosures: &[FeedEnclosure],
    site_url: &str,
    max_size: u64,
) {
    for enc in enclosures {
        // PHP: `preg_match("/(image|audio|video)/", $enc->type)` — a bare
        // substring gate (no slash).
        if ["image", "audio", "video"]
            .iter()
            .any(|k| enc.enc_type.contains(k))
        {
            cache_media_url(cache, &enc.link, site_url, max_size).await;
        }
    }
}

/// PHP `RSSUtils::labels_contains_caption($labels, $caption)` — true if any
/// label's caption (index 1) exactly equals `$caption` (case-sensitive).
pub fn labels_contains_caption(labels: &[Label], caption: &str) -> bool {
    labels.iter().any(|l| l.caption == caption)
}

/// `RSSUtils::update_basic_info($feed_id)` — populate the feed's `title` (only
/// if empty or `[Unknown]`) and `site_url` (only if it changed). The PHP first
/// runs the `HOOK_FEED_BASIC_INFO` plugin hooks to populate `$basic_info`,
/// short-circuiting on the first hook that returns a non-empty `title` or
/// `site_url`; only when none does falls back to the fetch + parse. A no-op
/// when the feed does not exist and, on the fetch fallback, when the fetch
/// returns nothing or the XML does not parse.
pub async fn update_basic_info(
    pool: &PgPool,
    cfg: &Config,
    plugins: &Arc<PluginHost>,
    feed_id: i32,
) {
    let row = sqlx::query(
        "SELECT feed_url, auth_login, title, site_url, auth_pass, owner_uid \
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
    let owner_uid = row.get::<i32, _>(5);

    let auth_pass_plaintext = decrypt_feed_pass(&auth_pass, cfg.encryption_key.as_ref());

    // HOOK_FEED_BASIC_INFO (M7.22 item 8): a plugin may supply the feed's
    // title/site_url, skipping the fetch. PHP `RSSUtils::update_basic_info`
    // (RSSUtils:311) — `run_hooks_callback(HOOK_FEED_BASIC_INFO, …)` short-circuits
    // on the first hook returning a non-empty `title` or `site_url`. No shipped
    // plugin implements it (af_comics's filters' `on_basic_info` returns
    // `false`), so it's a no-op for the stock set; the call site exists so a
    // community plugin works.
    let hook_basic: Option<(String, String)> = {
        let system: Vec<String> = cfg
            .plugins
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let mut ctx = crate::pluginhost::dispatch::owner_ctx_from(
            Arc::clone(plugins),
            pool.clone(),
            cfg.clone(),
            &system,
            &cfg.self_url,
            owner_uid,
        )
        .await;
        let host = ctx.host_arc();
        let mut out = None;
        for idx in ctx.hook_order(crate::pluginhost::HOOK_FEED_BASIC_INFO) {
            let mut bi = Value::Object(serde_json::Map::new());
            let result = host
                .plugin(idx)
                .hook_feed_basic_info(
                    &mut ctx,
                    &mut bi,
                    &feed_url,
                    owner_uid,
                    feed_id,
                    &auth_login,
                    &auth_pass_plaintext,
                )
                .await;
            let t = result
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|t| t.to_string());
            let s = result
                .get("site_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|t| t.to_string());
            if t.is_some() || s.is_some() {
                out = Some((t.unwrap_or_default(), s.unwrap_or_default()));
                break;
            }
        }
        out
    };

    // The basic info: from the hook (above) or, failing that, the fetch + parse.
    let (bi_title, bi_site_url): (String, String) = match hook_basic {
        Some((t, s)) => (t, s),
        None => {
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
                filtering: Some(true),
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
            (
                clean_str(&parser.get_title()).chars().take(199).collect(),
                rewrite_relative(&feed_url, &clean_str(&parser.get_link()))
                    .unwrap_or_default()
                    .chars()
                    .take(245)
                    .collect(),
            )
        }
    };

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
    // make_guid_hashed() — the stored `guid_hashed` (ttrss_entries.guid)
    // ------------------------------------------------------------------------

    #[test]
    fn test_make_guid_hashed_php_key_order() {
        // PHP: json_encode(["ver" => 2, "uid" => 2, "hash" => 'SHA1:' . sha1("abc")])
        //      = {"ver":2,"uid":2,"hash":"SHA1:<sha1("abc")>"}
        assert_eq!(
            make_guid_hashed(2, "abc"),
            format!(
                "{{\"ver\":2,\"uid\":2,\"hash\":\"SHA1:{}\"}}",
                crate::util::sha1_hex("abc")
            )
        );
        // Key order is (ver, uid, hash) — NOT serde_json's sorted (hash, uid, ver).
        assert!(make_guid_hashed(2, "abc").starts_with("{\"ver\":2,\"uid\":2,\"hash\":\""));
        // A different owner_uid flows through verbatim.
        assert_eq!(
            make_guid_hashed(10, "x"),
            format!(
                "{{\"ver\":2,\"uid\":10,\"hash\":\"SHA1:{}\"}}",
                crate::util::sha1_hex("x")
            )
        );
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

    // ------------------------------------------------------------------------
    // media_urls() — the media disk-caching extraction (PHP XPath)
    // ------------------------------------------------------------------------

    #[test]
    fn test_media_urls_img_src_and_data_skip() {
        let html = "<img src=\"http://ex.com/a.png\">\
                    <img src=\"data:image/png;base64,xx\">\
                    <img src=\"rel/b.png\">";
        let urls = media_urls(html, "http://ex.com/x/");
        assert_eq!(
            urls,
            vec![
                "http://ex.com/a.png".to_string(),
                "http://ex.com/x/rel/b.png".to_string(),
            ]
        );
    }

    #[test]
    fn test_media_urls_source_srcset_video_poster() {
        let html = "<video poster=\"http://ex.com/p.jpg\">\
                    <source src=\"http://ex.com/v.mp4\">\
                    <source srcset=\"http://ex.com/s1.jpg 1x, http://ex.com/s2.jpg 2x\">\
                   </video>";
        let urls = media_urls(html, "http://ex.com/");
        assert_eq!(
            urls,
            vec![
                "http://ex.com/p.jpg".to_string(),
                "http://ex.com/v.mp4".to_string(),
                "http://ex.com/s1.jpg".to_string(),
                "http://ex.com/s2.jpg".to_string(),
            ]
        );
    }

    #[test]
    fn test_media_urls_empty_html() {
        assert_eq!(media_urls("", "http://ex.com/"), Vec::<String>::new());
    }

    #[tokio::test]
    async fn test_cache_media_url_hit_short_circuits() {
        let base = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::new(base.path(), "images", "http://self");
        let url = "http://unreachable.invalid/a.png";
        cache.put(&sha1_hex(url), b"x");
        // An existing file short-circuits (no fetch is attempted).
        cache_media_url(&cache, url, "http://self", 1024).await;
        assert!(cache.exists(&sha1_hex(url)));
        // No file for a URL that is not cached (the fetch fails — the
        // unreachable host — so nothing is stored).
        cache_media_url(
            &cache,
            "http://unreachable.invalid/b.png",
            "http://self",
            1024,
        )
        .await;
        assert!(!cache.exists(&sha1_hex("http://unreachable.invalid/b.png")));
    }

    #[tokio::test]
    async fn test_cache_enclosure_type_gate() {
        let base = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::new(base.path(), "images", "http://self");
        // A `text/plain` enclosure is not media: no download is attempted
        // (unlike the image/audio/video gate), so nothing is stored even
        // though the (unreachable) URL would have been a no-op fetch.
        let encs = vec![
            FeedEnclosure {
                link: "http://unreachable.invalid/t.txt".into(),
                enc_type: "text/plain".into(),
                length: "1".into(),
                title: String::new(),
                height: "0".into(),
                width: "0".into(),
            },
            FeedEnclosure {
                link: "http://unreachable.invalid/v.mp4".into(),
                enc_type: "video/mp4".into(),
                length: "1".into(),
                title: String::new(),
                height: "0".into(),
                width: "0".into(),
            },
        ];
        cache_enclosures(&cache, &encs, "http://self", 1024).await;
        assert!(!cache.exists(&sha1_hex("http://unreachable.invalid/t.txt")));
        assert!(!cache.exists(&sha1_hex("http://unreachable.invalid/v.mp4")));
    }

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
