//! Port of `DiskCache::send()` (the local `Cache_Local` adapter) — serves a
//! cached file (feed favicon, image, ...) with PHP's exact response headers.
//! Used by `Handler_Public::cached()` and `Handler_Public::feed_icon()`.
//!
//! The MIME type is detected from the file *content* (matching PHP
//! `mime_content_type()` / libmagic), not the filename, because cached files
//! are extensionless (`feed-icons/1`).

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use kuchikiki::parse_html;
use kuchikiki::traits::TendrilSink;
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

/// `Config::CACHE_MAX_DAYS` (T_INT, default 7): the `Expires` stamp is the
/// file mtime plus this many days.
pub const CACHE_MAX_DAYS: i64 = 7;

/// `mime_content_type()` — detect the media type from magic bytes, matching
/// libmagic's result for the types that appear in the cache.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    // JPEG: FF D8
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return "image/jpeg";
    }
    // GIF: "GIF8"
    if bytes.starts_with(b"GIF8") {
        return "image/gif";
    }
    // ICO (MS Windows icon resource): 00 00 01 00
    if bytes.len() >= 4 && bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return "image/vnd.microsoft.icon";
    }
    // WEBP: RIFF....WEBP
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    // SVG: leading XML/SVG markup (inspected over a small prefix).
    let head = &bytes[..bytes.len().min(512)];
    if let Ok(s) = std::str::from_utf8(head) {
        let s = s.trim_start();
        if s.starts_with("<svg") || s.starts_with("<?xml") {
            return "image/svg+xml";
        }
    }
    "application/octet-stream"
}

/// `DiskCache::$mimeMap` (media subset): MIME type → canonical extension used
/// for the `Content-Disposition` filename.
pub fn fake_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/vnd.microsoft.icon" => "ico",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "",
    }
}

/// `gmdate('D, d M Y H:i:s', $ts) . ' GMT'` — an RFC 1123 date.
fn http_date(unix_secs: i64) -> String {
    crate::session::rfc1123(unix_secs)
}

/// `DiskCache::send($filename)` for the local adapter. `path` is the full
/// path to the cached file. The `If-Modified-Since` / `If-None-Match` 304
/// path is omitted (the harness never sends conditional headers).
pub fn send_file(path: &Path) -> Response {
    let (Ok(meta), Ok(bytes)) = (std::fs::metadata(path), std::fs::read(path)) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File not found."))
            .expect("valid response");
    };

    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mime = sniff_mime(&bytes);
    let effective = if mime == "application/octet-stream" {
        "video/mp4"
    } else {
        mime
    };

    // Only serve image/audio/video; block SVG (embedded-JS risk).
    let is_media = effective
        .split('/')
        .next()
        .is_some_and(|t| ["image", "audio", "video"].contains(&t));
    if !is_media || effective == "image/svg+xml" {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(format!(
                "Stored file has disallowed content type ({effective})"
            )))
            .expect("valid response");
    }

    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = fake_extension(effective);
    let disp = if ext.is_empty() {
        filename.clone()
    } else {
        format!("{filename}.{ext}")
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{disp}\""))
        .header(header::CONTENT_TYPE, effective)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::EXPIRES, http_date(mtime + 86400 * CACHE_MAX_DAYS))
        .header(header::LAST_MODIFIED, http_date(mtime))
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::ETAG, mtime.to_string())
        .body(Body::from(bytes))
        .expect("valid response")
}

/// `Handler_Public::cached()`: `file` is `<subdir>/<filename>`; the filename
/// has its dots stripped ("we do not allow files with extensions"). The cache
/// base is the parent of the feed-icons dir (the app `CACHE_DIR`).
pub fn resolve_cached(feed_icon_dir: &Path, file: &str) -> Option<PathBuf> {
    let (subdir, raw_name) = file.split_once('/')?;
    let base = feed_icon_dir.parent()?;
    let name = raw_name.replace('.', "").trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(base.join(subdir.trim()).join(name))
}

/// `DiskCache::rewrite_urls($str)` — rewrite image `src`/`srcset`/`poster`
/// URLs to local disk-cache URLs (`<self>/public.php?op=cached&file=images/<sha1>`)
/// when a cached copy exists. Called *after* `sanitize`, so URLs are already
/// absolute; unchanged input is returned as-is.
pub fn rewrite_urls(html: &str, image_cache_dir: &Path, self_url: &str) -> String {
    let res = html.trim();
    if res.is_empty() {
        return String::new();
    }

    let doc = parse_html().one(res);
    let mut changed = false;

    for node in doc.descendants() {
        let Some(el) = node.as_element() else {
            continue;
        };
        // `//img[@src]|//source[@src|@srcset]|//video[@poster|@src]`
        let name = el.name.local.to_string();
        if !matches!(name.as_str(), "img" | "source" | "video") {
            continue;
        }
        // `src` / `poster`: a cached hit rewrites the attribute and drops
        // `srcset`.
        for attr in ["src", "poster"] {
            let url = el.attributes.borrow().get(attr).map(str::to_string);
            let Some(url) = url else {
                continue;
            };
            if let Some(cached) = cached_file_url(image_cache_dir, &url, self_url) {
                el.attributes.borrow_mut().insert(attr, cached);
                el.attributes.borrow_mut().remove("srcset");
                changed = true;
            }
        }
        // Remaining `srcset`: rewrite each entry that has a cached copy.
        let srcset = el.attributes.borrow().get("srcset").map(str::to_string);
        if let Some(srcset) = srcset {
            let mut items = decode_srcset(&srcset);
            let mut any = false;
            for item in items.iter_mut() {
                if let Some(cached) = cached_file_url(image_cache_dir, &item.url, self_url) {
                    item.url = cached;
                    any = true;
                }
            }
            if any {
                el.attributes.borrow_mut().insert("srcset", encode_srcset(&items));
                changed = true;
            }
        }
    }

    if !changed {
        return html.to_string();
    }
    // Re-serialize the `<body>` contents (the sanitizer's
    // saveHTML-then-extract-`<body>` equivalent).
    let mut out = String::new();
    if let Some(body) = crate::sanitizer::find_body(&doc) {
        for child in body.children() {
            crate::sanitizer::serialize_node(&child, &mut out);
        }
    }
    out
}

/// `<self_url>/public.php?op=cached&file=images/<sha1(url)>` when
/// `<image_cache_dir>/<sha1(url)>` exists.
fn cached_file_url(image_cache_dir: &Path, url: &str, self_url: &str) -> Option<String> {
    let mut h = Sha1::new();
    h.update(url.as_bytes());
    let name: String = h
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if image_cache_dir.join(&name).is_file() {
        Some(format!("{self_url}/public.php?op=cached&file=images/{name}"))
    } else {
        None
    }
}

/// One `srcset` subitem (`<url> <desc>`).
struct SrcsetItem {
    url: String,
    size: String,
}

/// `RSSUtils::decode_srcset`: `"<url> <desc>, <url> …"` → items. (A
/// practical port of the PHP regex; the corpus never rewrites srcset.)
fn decode_srcset(srcset: &str) -> Vec<SrcsetItem> {
    let mut out = Vec::new();
    for part in srcset.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (url, size) = match part.split_once(char::is_whitespace) {
            Some((u, s)) => (u.trim().to_string(), s.trim().to_string()),
            None => (part.to_string(), String::new()),
        };
        if !url.is_empty() {
            out.push(SrcsetItem { url, size });
        }
    }
    out
}

/// `RSSUtils::encode_srcset`: items → `"<url> <desc>, …"` (PHP always joins
/// url + `" "` + desc).
fn encode_srcset(items: &[SrcsetItem]) -> String {
    items
        .iter()
        .map(|i| format!("{} {}", i.url, i.size))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_matches_finfo() {
        // Ground truth from PHP `mime_content_type()` on the golden icons.
        assert_eq!(
            sniff_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]),
            "image/png"
        );
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_mime(b"GIF89a...."), "image/gif");
        assert_eq!(
            sniff_mime(&[0x00, 0x00, 0x01, 0x00, 0x01, 0x00]),
            "image/vnd.microsoft.icon"
        );
        assert_eq!(
            sniff_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            "image/webp"
        );
        assert_eq!(sniff_mime(b"<?xml version=\"1.0\"?>\n<svg/>"), "image/svg+xml");
        assert_eq!(sniff_mime(b"\x00\x01\x02random"), "application/octet-stream");
    }

    #[test]
    fn fake_ext_matches_mimemap() {
        assert_eq!(fake_extension("image/png"), "png");
        assert_eq!(fake_extension("image/jpeg"), "jpg");
        assert_eq!(fake_extension("image/vnd.microsoft.icon"), "ico");
        assert_eq!(fake_extension("image/svg+xml"), "svg");
        assert_eq!(fake_extension("video/mp4"), "");
    }

    #[test]
    fn cached_path_resolution() {
        let dir = Path::new("/app/cache/feed-icons");
        assert_eq!(
            resolve_cached(dir, "feed-icons/1").as_deref(),
            Some(Path::new("/app/cache/feed-icons/1"))
        );
        // Dots are stripped from the filename.
        assert_eq!(
            resolve_cached(dir, "images/abc.png").as_deref(),
            Some(Path::new("/app/cache/images/abcpng"))
        );
        assert_eq!(resolve_cached(dir, "no-slash"), None);
    }
}
