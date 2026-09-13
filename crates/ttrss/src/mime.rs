//! Content-Type mapping for static assets, matching the oracle's nginx
//! `mime.types` exactly (this is the canonical source, not `mime_guess`, which
//! e.g. reports `text/javascript` where nginx serves `application/javascript`).
//! Unknown extensions fall back to `application/octet-stream` (nginx's
//! `default_type`).

use std::path::Path;

/// Content-Type for a path based on its file extension (nginx `mime.types`).
pub fn mime_for_path(path: &Path) -> &'static str {
    path.extension()
        .and_then(|e| e.to_str())
        .map(mime_for_ext)
        .unwrap_or("application/octet-stream")
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html",
        "htm" => "text/html",
        "shtml" => "text/html",
        "css" => "text/css",
        "xml" => "text/xml",
        "gif" => "image/gif",
        "jpeg" => "image/jpeg",
        "jpg" => "image/jpeg",
        "js" => "application/javascript",
        "atom" => "application/atom+xml",
        "rss" => "application/rss+xml",
        "mml" => "text/mathml",
        "txt" => "text/plain",
        "jad" => "text/vnd.sun.j2me.app-descriptor",
        "wml" => "text/vnd.wap.wml",
        "htc" => "text/x-component",
        "avif" => "image/avif",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "svgz" => "image/svg+xml",
        "tif" => "image/tiff",
        "tiff" => "image/tiff",
        "wbmp" => "image/vnd.wap.wbmp",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "jng" => "image/x-jng",
        "bmp" => "image/x-ms-bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "jar" => "application/java-archive",
        "war" => "application/java-archive",
        "ear" => "application/java-archive",
        "json" => "application/json",
        "hqx" => "application/mac-binhex40",
        "doc" => "application/msword",
        "pdf" => "application/pdf",
        "ps" => "application/postscript",
        "eps" => "application/postscript",
        "ai" => "application/postscript",
        "rtf" => "application/rtf",
        "m3u8" => "application/vnd.apple.mpegurl",
        "kml" => "application/vnd.google-earth.kml+xml",
        "kmz" => "application/vnd.google-earth.kmz",
        "xls" => "application/vnd.ms-excel",
        "eot" => "application/vnd.ms-fontobject",
        "ppt" => "application/vnd.ms-powerpoint",
        "odg" => "application/vnd.oasis.opendocument.graphics",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "wmlc" => "application/vnd.wap.wmlc",
        "wasm" => "application/wasm",
        "7z" => "application/x-7z-compressed",
        "cco" => "application/x-cocoa",
        "jardiff" => "application/x-java-archive-diff",
        "jnlp" => "application/x-java-jnlp-file",
        "run" => "application/x-makeself",
        "pl" => "application/x-perl",
        "pm" => "application/x-perl",
        "prc" => "application/x-pilot",
        "pdb" => "application/x-pilot",
        "rar" => "application/x-rar-compressed",
        "rpm" => "application/x-redhat-package-manager",
        "sea" => "application/x-sea",
        "swf" => "application/x-shockwave-flash",
        "sit" => "application/x-stuffit",
        "tcl" => "application/x-tcl",
        "tk" => "application/x-tcl",
        "der" => "application/x-x509-ca-cert",
        "pem" => "application/x-x509-ca-cert",
        "crt" => "application/x-x509-ca-cert",
        "xpi" => "application/x-xpinstall",
        "xhtml" => "application/xhtml+xml",
        "xspf" => "application/xspf+xml",
        "zip" => "application/zip",
        "bin" => "application/octet-stream",
        "exe" => "application/octet-stream",
        "dll" => "application/octet-stream",
        "deb" => "application/octet-stream",
        "dmg" => "application/octet-stream",
        "iso" => "application/octet-stream",
        "img" => "application/octet-stream",
        "msi" => "application/octet-stream",
        "msp" => "application/octet-stream",
        "msm" => "application/octet-stream",
        "mid" => "audio/midi",
        "midi" => "audio/midi",
        "kar" => "audio/midi",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "m4a" => "audio/x-m4a",
        "ra" => "audio/x-realaudio",
        "3gpp" => "video/3gpp",
        "3gp" => "video/3gpp",
        "ts" => "video/mp2t",
        "mp4" => "video/mp4",
        "mpeg" => "video/mpeg",
        "mpg" => "video/mpeg",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "flv" => "video/x-flv",
        "m4v" => "video/x-m4v",
        "mng" => "video/x-mng",
        "asx" => "video/x-ms-asf",
        "asf" => "video/x-ms-asf",
        "wmv" => "video/x-ms-wmv",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_nginx() {
        assert_eq!(
            mime_for_path(Path::new("js/common.js")),
            "application/javascript"
        );
        assert_eq!(mime_for_path(Path::new("themes/light.css")), "text/css");
        assert_eq!(
            mime_for_path(Path::new("images/three-dots.svg")),
            "image/svg+xml"
        );
        assert_eq!(mime_for_path(Path::new("fonts/x.woff2")), "font/woff2");
        assert_eq!(
            mime_for_path(Path::new("data.bin")),
            "application/octet-stream"
        );
    }
}
