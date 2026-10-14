//! Port of `DiskCache::send()` (the local `Cache_Local` adapter) — serves a
//! cached file (feed favicon, image, ...) with PHP's exact response headers.
//! Used by `Handler_Public::cached()` and `Handler_Public::feed_icon()`.
//!
//! The MIME type is detected from the file *content* (matching PHP
//! `mime_content_type()` / libmagic), not the filename, because cached files
//! are extensionless (`feed-icons/1`).

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use kuchikiki::parse_html;
use kuchikiki::traits::TendrilSink;
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
    // WEBP: RIFF....WEBP; WAV: RIFF....WAVE (libmagic: audio/wav).
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        if &bytes[8..12] == b"WEBP" {
            return "image/webp";
        }
        if &bytes[8..12] == b"WAVE" {
            return "audio/wav";
        }
    }
    // MP4/MOV: 4-byte size + "ftyp" (libmagic: video/mp4, or
    // video/quicktime for the "qt  " major brand).
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return if &bytes[8..12] == b"qt  " {
            "video/quicktime"
        } else {
            "video/mp4"
        };
    }
    // OGG: "OggS"; FLAC: "fLaC"; MP3: "ID3" tag or a frame-sync header.
    if bytes.starts_with(b"OggS") {
        return "audio/ogg";
    }
    if bytes.starts_with(b"fLaC") {
        return "audio/flac";
    }
    if bytes.len() >= 3
        && (bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0))
    {
        return "audio/mpeg";
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
/// for the `Content-Disposition` filename. `send_file` passes the *pre-substitution*
/// mime (the reference re-detects the file for the lookup, so a file served under
/// the `video/mp4` fallback looks up `application/octet-stream`).
pub fn fake_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/vnd.microsoft.icon" => "ico",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        // PHP's map entry verbatim: files served under the `video/mp4`
        // fallback keep the `.pdf` name (the reference's quirk).
        "application/octet-stream" => "pdf",
        _ => "",
    }
}

/// `gmdate('D, d M Y H:i:s', $ts) . ' GMT'` — an RFC 1123 date.
fn http_date(unix_secs: i64) -> String {
    crate::session::rfc1123(unix_secs)
}

/// `DiskCache::send($filename)` for the local adapter. `path` is the full
/// path to the cached file; `req` carries the request's conditional
/// headers. PHP compares `If-Modified-Since` / `If-None-Match` with **exact
/// string equality** against the `Last-Modified` stamp and the *unquoted*
/// mtime ETag, and answers 304 (no body, cache headers only) when either
/// matches.
pub fn send_file(path: &Path, req: &HeaderMap) -> Response {
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
    let last_modified = http_date(mtime);
    let expires = http_date(mtime + 86400 * CACHE_MAX_DAYS);
    let etag = mtime.to_string();

    // `If-Modified-Since == $gmt_modified || If-None-Match == $file_mtime`.
    let ims = req
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok());
    let inm = req.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    if ims == Some(last_modified.as_str()) || inm == Some(etag.as_str()) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::EXPIRES, expires)
            .header(header::LAST_MODIFIED, last_modified)
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::ETAG, etag)
            .body(Body::empty())
            .expect("valid response");
    }

    let mime = sniff_mime(&bytes);
    let effective = if mime == "application/octet-stream" {
        "video/mp4"
    } else {
        mime
    };

    // Only serve image/audio/video. Deliberate deviation: the reference
    // blacklists `image/svg+xml` here (400); the port serves SVG like any
    // other image (it is a supported port feature).
    let is_media = effective
        .split('/')
        .next()
        .is_some_and(|t| ["image", "audio", "video"].contains(&t));
    if !is_media {
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
    // The reference looks up the mimeMap with the freshly re-detected (i.e.
    // pre-substitution) mime.
    let ext = fake_extension(mime);
    let disp = if ext.is_empty() {
        filename.clone()
    } else {
        format!("{filename}.{ext}")
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{disp}\""),
        )
        .header(header::CONTENT_TYPE, effective)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::EXPIRES, expires)
        .header(header::LAST_MODIFIED, last_modified)
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::ETAG, etag)
        .body(Body::from(bytes))
        .expect("valid response")
}

/// `Handler_Public::cached()`: `file` is `<subdir>/<filename>`; the filename
/// has its dots stripped ("we do not allow files with extensions"). The cache
/// base is the parent of the feed-icons dir (the app `CACHE_DIR`).
pub fn resolve_cached(cache_dir: &Path, file: &str) -> Option<PathBuf> {
    // `explode("/", file, 2)`: first component = subdir, the rest = filename.
    let (subdir, raw_name) = file.split_once('/')?;
    // PHP strips the dots FIRST, then applies `basename()` on every adapter
    // call — the basename drops the separators that survive, so
    // `file=images/../../feed-icons/123` resolves to `images/123`, never
    // outside the subdir (the route is unauthenticated).
    let stripped = raw_name.replace('.', "");
    let name = php_basename(&stripped);
    if name.is_empty() {
        return None;
    }
    // PHP `set_dir`: `basename(clean($dir))` — `clean()` = strip_tags + trim.
    let subdir = crate::util::clean_str(subdir);
    Some(cache_dir.join(php_basename(&subdir)).join(name))
}

/// PHP `basename()`: the last path component — unlike `Path::file_name`,
/// also yields a component for trailing-slash names (`basename("a/b/") ==
/// "b"`), and `""` for `""`/`"/"`.
fn php_basename(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(i) => &t[i + 1..],
        None => t,
    }
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
                el.attributes
                    .borrow_mut()
                    .insert("srcset", encode_srcset(&items));
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
    let name = crate::util::sha1_hex(url);
    if image_cache_dir.join(&name).is_file() {
        Some(format!(
            "{self_url}/public.php?op=cached&file=images/{name}"
        ))
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

/// `DiskCache` + its local (filesystem) adapter (classes/DiskCache.php,
/// classes/Cache_Local.php) — the *storage* side: `exists`/`get`/`put`/
/// `remove`/`is_writable`/`get_dir`/`get_mtime`/`get_size`/`download` +
/// `get_url`. The serve side is [`send_file`]. M7.12 (`cache_starred_images`)
/// is the first consumer.
///
/// A `DiskCache` names one `<cache_dir>/<name>` subdirectory (upstream's
/// `DiskCache::instance($name)`). The port builds one per use — it holds only
/// paths, so no singleton is needed (the PHP's per-request singleton is a
/// no-op across the port's stateless plugins).
#[derive(Clone, Debug)]
pub struct DiskCache {
    /// `<cache_dir>/<name>` — the instance directory.
    dir: PathBuf,
    /// `Config::get_self_url()` (rtrimmed) — the base for [`DiskCache::get_url`].
    self_url: String,
    /// The bare instance name (e.g. `"starred-images"`) — the `file=<name>/…`
    /// path segment in [`DiskCache::get_url`].
    name: String,
}

impl DiskCache {
    /// `DiskCache::instance($name)` over a base `<cache_dir>`: `<cache_dir>/<name>`.
    /// Creates the directory if missing (upstream `Cache_Local::set_dir` →
    /// `make_dir`).
    pub fn new(base_dir: &Path, name: &str, self_url: &str) -> Self {
        let dir = base_dir.join(name);
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            self_url: self_url.trim_end_matches('/').to_string(),
            name: name.to_string(),
        }
    }

    /// `Cache_Local::get_full_path($filename)` — the directory + the filename.
    /// (Upstream applies `basename(clean(…))`; the filenames here are already
    /// clean basenames, so a plain join.)
    pub fn get_full_path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }

    /// `Cache_Local::exists($filename)`.
    pub fn exists(&self, filename: &str) -> bool {
        self.get_full_path(filename).is_file()
    }

    /// `Cache_Local::get($filename)` — the file contents, or `None` if absent.
    /// (Text-oriented; the port's only consumer reads JSON status files.)
    pub fn get(&self, filename: &str) -> Option<String> {
        std::fs::read_to_string(self.get_full_path(filename)).ok()
    }

    /// `Cache_Local::put($filename, $data)` — writes the bytes; `true` on success.
    pub fn put(&self, filename: &str, data: &[u8]) -> bool {
        std::fs::write(self.get_full_path(filename), data).is_ok()
    }

    /// `Cache_Local::remove($filename)`.
    pub fn remove(&self, filename: &str) -> bool {
        std::fs::remove_file(self.get_full_path(filename)).is_ok()
    }

    /// `Cache_Local::is_writable($filename)`.
    ///
    /// Upstream asks the OS for write permission; the port approximates it with
    /// existence (the directory is created by [`DiskCache::new`]), which is all
    /// the consumers use it as (a gate before writing).
    pub fn is_writable(&self, filename: Option<&str>) -> bool {
        match filename {
            Some(f) if self.get_full_path(f).exists() => self.get_full_path(f).is_file(),
            _ => self.dir.is_dir(),
        }
    }

    /// `Cache_Local::get_dir()`.
    pub fn get_dir(&self) -> &Path {
        &self.dir
    }

    /// `Cache_Local::get_mtime($filename)` — Unix seconds, or `-1` if absent.
    pub fn get_mtime(&self, filename: &str) -> i64 {
        std::fs::metadata(self.get_full_path(filename))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(-1)
    }

    /// `Cache_Local::get_size($filename)` — byte count, or `-1` if absent.
    pub fn get_size(&self, filename: &str) -> i64 {
        if self.exists(filename) {
            self.get_full_path(filename)
                .metadata()
                .map(|m| m.len() as i64)
                .unwrap_or(-1)
        } else {
            -1
        }
    }

    /// `DiskCache::get_url($filename)` — the public URL that serves a cached
    /// file (`<self>/public.php?op=cached&file=<name>/<filename>`).
    pub fn get_url(&self, filename: &str) -> String {
        format!(
            "{}/public.php?op=cached&file={}/{}",
            self.self_url, self.name, filename
        )
    }

    /// `DiskCache::download($url, $local_filename, $force)` — fetch `$url` and
    /// store it if absent (or `$force`d). `max_size` caps the download
    /// (`Config::MAX_CACHE_FILE_SIZE`).
    pub async fn download(
        &self,
        url: &str,
        local_filename: &str,
        max_size: u64,
        force: bool,
    ) -> bool {
        if self.exists(local_filename) && !force {
            return true;
        }
        let result = crate::urlhelper::fetch(&crate::urlhelper::FetchOptions {
            url: url.to_string(),
            max_size: Some(max_size),
            ..Default::default()
        })
        .await;
        match result.body {
            Some(bytes) if !bytes.is_empty() => self.put(local_filename, &bytes),
            _ => false,
        }
    }
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
        assert_eq!(sniff_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "), "image/webp");
        assert_eq!(
            sniff_mime(b"<?xml version=\"1.0\"?>\n<svg/>"),
            "image/svg+xml"
        );
        // Media: MP4/MOV (ftyp box), MP3 (ID3 + frame sync), OGG, FLAC, WAV.
        let mut mp4 = vec![0, 0, 0, 28];
        mp4.extend_from_slice(b"ftypisom");
        assert_eq!(sniff_mime(&mp4), "video/mp4");
        let mut mov = vec![0, 0, 0, 28];
        mov.extend_from_slice(b"ftypqt  ");
        assert_eq!(sniff_mime(&mov), "video/quicktime");
        assert_eq!(sniff_mime(b"ID3\x04\x00\x00\x00\x00\x00"), "audio/mpeg");
        assert_eq!(sniff_mime(&[0xFF, 0xFB, 0x90, 0x00, 0, 0]), "audio/mpeg");
        assert_eq!(sniff_mime(b"OggS\x00\x02\x00\x00"), "audio/ogg");
        assert_eq!(sniff_mime(b"fLaC\x00\x00\x00\x00"), "audio/flac");
        let mut wav = vec![0u8; 12];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        assert_eq!(sniff_mime(&wav), "audio/wav");
        assert_eq!(
            sniff_mime(b"\x00\x01\x02random"),
            "application/octet-stream"
        );
    }

    #[test]
    fn fake_ext_matches_mimemap() {
        assert_eq!(fake_extension("image/png"), "png");
        assert_eq!(fake_extension("image/jpeg"), "jpg");
        assert_eq!(fake_extension("image/vnd.microsoft.icon"), "ico");
        assert_eq!(fake_extension("image/svg+xml"), "svg");
        assert_eq!(fake_extension("video/mp4"), "mp4");
        assert_eq!(fake_extension("video/quicktime"), "mov");
        assert_eq!(fake_extension("audio/mpeg"), "mp3");
        assert_eq!(fake_extension("audio/ogg"), "ogg");
        assert_eq!(fake_extension("audio/flac"), "flac");
        assert_eq!(fake_extension("audio/wav"), "wav");
        // The reference's map quirk: the `video/mp4` fallback serves `.pdf`.
        assert_eq!(fake_extension("application/octet-stream"), "pdf");
        assert_eq!(fake_extension("text/plain"), "");
    }

    #[test]
    fn cached_path_resolution() {
        let dir = Path::new("/app/cache");
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

    #[test]
    fn cached_path_resolution_blocks_traversal() {
        // PHP: dots stripped, then `basename()` on every adapter call — the
        // remainder can never escape the subdir (unauthenticated route).
        let dir = Path::new("/app/cache");
        assert_eq!(
            resolve_cached(dir, "images/../../feed-icons/123").as_deref(),
            Some(Path::new("/app/cache/images/123"))
        );
        // Trailing slash: PHP `basename("abc/") == "abc"` (a file IS served).
        assert_eq!(
            resolve_cached(dir, "images/abc/").as_deref(),
            Some(Path::new("/app/cache/images/abc"))
        );
        // All dots → empty name → 404 (PHP `basename("") == ""`).
        assert_eq!(resolve_cached(dir, "images/..."), None);
        // PHP does NOT trim the filename — spaces survive (parity, no trim).
        assert_eq!(
            resolve_cached(dir, "images/  abc").as_deref(),
            Some(Path::new("/app/cache/images/  abc"))
        );
        // PHP `set_dir` applies `clean()` (strip_tags + trim) to the subdir.
        assert_eq!(
            resolve_cached(dir, "<img>/123").as_deref(),
            Some(Path::new("/app/cache/123"))
        );
    }

    static TMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A fresh temp dir with cached copies for each URL in `urls`.
    fn temp_cache(urls: &[&str]) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ttrss-rewrite-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        for url in urls {
            std::fs::write(dir.join(crate::util::sha1_hex(url)), b"cached").unwrap();
        }
        dir
    }

    #[test]
    fn rewrite_urls_rewrites_cached_img_src() {
        let dir = temp_cache(&["http://a.com/1.jpg", "http://b.com/2.jpg"]);
        let html = r#"<img src="http://a.com/1.jpg"><img src="http://b.com/2.jpg">"#;
        let res = rewrite_urls(html, &dir, "http://self");
        assert!(res.contains("public.php"), "rewritten: {res}");
        assert!(
            res.contains(&crate::util::sha1_hex("http://a.com/1.jpg")),
            "first img: {res}"
        );
        assert!(
            res.contains(&crate::util::sha1_hex("http://b.com/2.jpg")),
            "second img: {res}"
        );
    }

    #[test]
    fn rewrite_urls_rewrites_video_poster() {
        let dir = temp_cache(&["http://example.com/poster.jpg"]);
        let html = r#"<video poster="http://example.com/poster.jpg"></video>"#;
        let res = rewrite_urls(html, &dir, "http://self");
        assert!(
            res.contains(&crate::util::sha1_hex("http://example.com/poster.jpg")),
            "poster: {res}"
        );
    }

    #[test]
    fn rewrite_urls_rewrites_video_src() {
        let dir = temp_cache(&["http://example.com/video.mp4"]);
        let html = r#"<video src="http://example.com/video.mp4"></video>"#;
        let res = rewrite_urls(html, &dir, "http://self");
        assert!(
            res.contains(&crate::util::sha1_hex("http://example.com/video.mp4")),
            "video src: {res}"
        );
    }

    #[test]
    fn rewrite_urls_no_cache_no_rewrite() {
        let dir = temp_cache(&[]);
        let html = r#"<img src="http://a.com/1.jpg">"#;
        let res = rewrite_urls(html, &dir, "http://self");
        assert_eq!(res, html, "no cached copy → unchanged: {res}");
    }

    #[test]
    fn rewrite_urls_preserves_non_cacheable_urls() {
        let dir = temp_cache(&[]);
        let html = "<p>Some text content with no images</p>";
        let res = rewrite_urls(html, &dir, "http://self");
        assert!(
            res.contains("Some text content with no images"),
            "text: {res}"
        );
    }

    #[test]
    fn rewrite_urls_empty_string() {
        let dir = temp_cache(&[]);
        assert_eq!(rewrite_urls("", &dir, "http://self"), "");
    }

    #[test]
    fn rewrite_urls_whitespace_only() {
        let dir = temp_cache(&[]);
        assert_eq!(rewrite_urls("  \n\t  ", &dir, "http://self"), "");
    }

    #[test]
    fn rewrite_urls_preserves_html_structure() {
        let dir = temp_cache(&["http://example.com/img.png"]);
        let html = r#"<div><p><img src="http://example.com/img.png"></p></div>"#;
        let res = rewrite_urls(html, &dir, "http://self");
        assert!(res.contains("<div>"), "div: {res}");
        assert!(res.contains("<p>"), "p: {res}");
        assert!(res.contains("</div>"), "closing div: {res}");
        assert!(
            res.contains(&crate::util::sha1_hex("http://example.com/img.png")),
            "img: {res}"
        );
    }

    #[test]
    fn rewrite_urls_removes_srcset_when_rewriting() {
        // A cached `src` also drops the element's `srcset`.
        let url = "http://example.com/pic.jpg";
        let dir = temp_cache(&[url]);
        let html = format!(r#"<img src="{url}" srcset="http://a.com/1x.jpg 1x">"#);
        let res = rewrite_urls(&html, &dir, "http://self");
        assert!(!res.contains("srcset"), "srcset dropped: {res}");
        assert!(
            res.contains(&crate::util::sha1_hex(url)),
            "src rewritten: {res}"
        );
    }
    // -- DiskCache storage (M7.12) ------------------------------------------

    /// A fresh empty temp dir (the base for a `DiskCache` instance).
    fn temp_base_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("ttrss-diskcache-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn diskcache_put_get_exists_roundtrip() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self/");
        assert!(!cache.exists("abc"));
        assert!(cache.put("abc", b"hello world"));
        assert!(cache.exists("abc"));
        assert_eq!(cache.get("abc").as_deref(), Some("hello world"));
        assert_eq!(cache.get("missing"), None);
    }

    #[test]
    fn diskcache_get_url_format() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self/");
        assert_eq!(
            cache.get_url("123-abc.status"),
            "http://self/public.php?op=cached&file=starred-images/123-abc.status"
        );
    }

    #[test]
    fn diskcache_remove() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self");
        cache.put("abc", b"x");
        assert!(cache.exists("abc"));
        assert!(cache.remove("abc"));
        assert!(!cache.exists("abc"));
        // Removing a missing file fails.
        assert!(!cache.remove("nope"));
    }

    #[test]
    fn diskcache_get_size_and_mtime() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self");
        cache.put("abc", b"12345");
        assert_eq!(cache.get_size("abc"), 5);
        assert_eq!(cache.get_size("missing"), -1);
        assert!(
            cache.get_mtime("abc") > 0,
            "mtime: {}",
            cache.get_mtime("abc")
        );
        assert_eq!(cache.get_mtime("missing"), -1);
    }

    #[test]
    fn diskcache_is_writable_and_dir() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self");
        assert!(cache.is_writable(None), "the created dir is writable");
        assert!(cache.get_dir().ends_with("starred-images"));
        // A present file is writable (the existence approximation).
        cache.put("abc", b"x");
        assert!(cache.is_writable(Some("abc")));
    }

    #[tokio::test]
    async fn diskcache_download_early_return_when_cached() {
        let base = temp_base_dir();
        let cache = DiskCache::new(&base, "starred-images", "http://self");
        cache.put("abc", b"cached");
        // An existing file short-circuits (no fetch) with `force = false`, even
        // for an unreachable URL (proving no network is attempted).
        assert!(
            cache
                .download("http://unreachable.invalid/x", "abc", 1024, false)
                .await
        );
    }

    #[test]
    fn send_file_serves_svg() {
        // SVG was intentionally blocked here; it must now serve like any
        // other image (favicon extension, see favicons::calculate_avg_color).
        let path = std::env::temp_dir().join("test_send_file.svg");
        std::fs::write(&path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
        let resp = send_file(&path, &HeaderMap::new());
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/svg+xml")
        );
        let _ = std::fs::remove_file(&path);
    }
}
