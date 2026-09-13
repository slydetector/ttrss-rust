//! Port of `tt-rss/classes/RSSUtils.php` — the feed-update engine helpers.
//!
//! **M6a.4** adds the content-encoding / gzip helpers. The rest of the class —
//! the deterministic update pipeline (fetch, hash, dedup, upsert, filter,
//! counters) — lands in **M6b**.
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
}
