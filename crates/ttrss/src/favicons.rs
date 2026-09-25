//! Feed-icon (favicon) download + avg-color calculation.
//!
//! Ports the PHP's `RSSUtils::get_favicon_urls`, `RSSUtils::update_favicon`,
//! and `Colors::calculate_avg_color`. The favicon check is triggered from
//! `process_feed` after the site_url is determined (RSSUtils.php:648-697).

use crate::core::config::Config;
use crate::urlhelper::{self, FetchOptions};
use kuchikiki::parse_html;
use kuchikiki::traits::TendrilSink;
use std::path::Path;

/// `RSSUtils::FAVICON_ALLOWED_MIME_TYPES` (RSSUtils.php:9-15).
const FAVICON_ALLOWED_MIME: &[&str] = &[
    "image/bmp",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/vnd.microsoft.icon",
];

/// `RSSUtils::get_favicon_urls` (RSSUtils.php:1954-1993) — fetch the site's
/// HTML, extract candidate favicon URLs from `<link>` tags, resolve relative
/// URLs, and fall back to `<base>/favicon.ico`.
pub async fn get_favicon_urls(site_url: &str) -> Vec<String> {
    // Fetch the site's HTML (no explicit max_size in the PHP; cap at 2 MiB).
    let result = urlhelper::fetch(&FetchOptions {
        url: site_url.to_string(),
        max_size: Some(2 * 1024 * 1024),
        ..Default::default()
    })
    .await;

    let Some(html_bytes) = result.body else {
        return vec![];
    };

    // The PHP's `loadHTML` is lenient about encoding; we do the same.
    let html = String::from_utf8_lossy(&html_bytes).to_string();
    let doc = parse_html().one(html);

    // Resolve the `<base href>` (if present).
    let base = doc
        .select("head > base[href]")
        .ok()
        .and_then(|mut sel| sel.next())
        .and_then(|el| el.attributes.borrow().get("href").map(|s| s.to_string()))
        .and_then(|href| urlhelper::rewrite_relative(site_url, &href))
        .unwrap_or_else(|| site_url.to_string());

    // Collect `<link rel="shortcut icon|icon|alternate icon">` entries.
    let mut urls: Vec<String> = Vec::new();
    if let Ok(sel) = doc.select("head > link[rel]") {
        for el in sel {
            let attrs = el.attributes.borrow();
            let rel = attrs.get("rel").map(|r| r.to_lowercase()).unwrap_or_default();
            if (rel == "shortcut icon" || rel == "icon" || rel == "alternate icon")
                && let Some(href) = attrs.get("href")
                    && let Some(abs) = urlhelper::rewrite_relative(&base, href) {
                        urls.push(abs);
                    }
        }
    }

    // Fallback: `<base>/favicon.ico`.
    if urls.is_empty()
        && let Some(fallback) = urlhelper::rewrite_relative(&base, "/favicon.ico") {
            urls.push(fallback);
        }

    urls
}

/// `RSSUtils::update_favicon` (RSSUtils.php:1774-1822) — try each candidate
/// URL: fetch, sniff MIME, save to disk if allowed. Returns `true` on success.
pub async fn update_favicon(cfg: &Config, site_url: &str, feed_id: i32) -> bool {
    let candidate_urls = get_favicon_urls(site_url).await;
    if candidate_urls.is_empty() {
        tracing::debug!(site_url, "favicon: no candidate URLs found");
        return false;
    }

    for url in &candidate_urls {
        let result = urlhelper::fetch(&FetchOptions {
            url: url.clone(),
            max_size: Some(cfg.max_favicon_file_size as u64),
            ..Default::default()
        })
        .await;

        let Some(body) = result.body else {
            continue;
        };

        let mime = crate::diskcache::sniff_mime(&body);
        if !FAVICON_ALLOWED_MIME.contains(&mime) {
            continue;
        }

        // Save to the feed-icon cache directory.
        let dir = &cfg.feed_icon_dir();
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let path = dir.join(feed_id.to_string());
        match std::fs::write(&path, &body) {
            Ok(_) => {
                tracing::debug!(feed_id, url, mime, "favicon: saved");
                return true;
            }
            Err(e) => {
                tracing::warn!(feed_id, url, error = %e, "favicon: write failed");
                return false;
            }
        }
    }

    false
}

/// `Colors::calculate_avg_color` (colors.php:274-295) — decode the icon,
/// sample pixels at granularity 4, quantize each channel to 0x33 (51) steps,
/// find the top-4 most frequent colors, and return the first that passes the
/// HSV threshold (s > 0.25, v > 0.25, not near-red, not near-white).
/// Returns `None` if no suitable color is found.
pub fn calculate_avg_color(icon_path: &Path) -> Option<String> {
    let bytes = std::fs::read(icon_path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return None;
    }

    // Convert to RGBA for uniform pixel access.
    let rgba = img.to_rgba8();

    // Build a frequency map: quantized color → count.
    let mut colors: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let [r, g, b, _] = rgba.get_pixel(x, y).0;
            let qr = (f64::from(r) / 51.0).round() * 51.0;
            let qg = (f64::from(g) / 51.0).round() * 51.0;
            let qb = (f64::from(b) / 51.0).round() * 51.0;
            let key = [
                qr.min(255.0) as u8,
                qg.min(255.0) as u8,
                qb.min(255.0) as u8,
            ];
            *colors.entry(key).or_insert(0) += 1;
        }
    }

    // Sort by frequency desc, take top 4.
    let mut items: Vec<([u8; 3], u32)> = colors.into_iter().collect();
    items.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    items.truncate(4);
    let palette: Vec<[u8; 3]> = items.into_iter().map(|(color, _)| color).collect();

    // For each color in the palette, convert to HSV and check the threshold.
    for [r, g, b] in &palette {
        let (hue, s, v) = rgb_to_hsv(*r, *g, *b);
        if s > 0.25
            && v > 0.25
            && !(hue < 0.01 && s < 0.01)
            && !(hue < 0.01 && v > 0.99)
        {
            return Some(format!("#{r:02x}{g:02x}{b:02x}"));
        }
    }

    None
}

/// Convert RGB (0-255) to HSV (h: 0-1, s: 0-1, v: 0-1).
/// Mirrors the PHP's `rgb2hsl` (colors.php:159-194) which despite the name
/// actually computes HSV (the `v` component is `var_Max` = max(r,g,b)/255).
fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let r = f64::from(r) / 255.0;
    let g = f64::from(g) / 255.0;
    let b = f64::from(b) / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        (((g - b) / delta) / 6.0 + 1.0).rem_euclid(1.0)
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };

    (h, s, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_hsv_red() {
        let (h, s, v) = rgb_to_hsv(255, 0, 0);
        assert!(h < 0.01);
        assert!(s > 0.99);
        assert!(v > 0.99);
    }

    #[test]
    fn test_rgb_to_hsv_green() {
        let (h, s, v) = rgb_to_hsv(0, 255, 0);
        assert!((h - 0.333).abs() < 0.01);
        assert!(s > 0.99);
        assert!(v > 0.99);
    }

    #[test]
    fn test_rgb_to_hsv_blue() {
        let (h, s, v) = rgb_to_hsv(0, 0, 255);
        assert!((h - 0.667).abs() < 0.01);
        assert!(s > 0.99);
        assert!(v > 0.99);
    }

    #[test]
    fn test_rgb_to_hsv_white() {
        let (h, s, v) = rgb_to_hsv(255, 255, 255);
        assert!(h < 0.01);
        assert!(s < 0.01);
        assert!(v > 0.99);
    }

    #[test]
    fn test_rgb_to_hsv_gray() {
        let (h, s, v) = rgb_to_hsv(128, 128, 128);
        assert!(h < 0.01);
        assert!(s < 0.01);
        assert!((v - 0.502).abs() < 0.01);
    }

    #[test]
    fn test_calculate_avg_color_solid_blue() {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 255, 255]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();

        let path = std::env::temp_dir().join("test_favicon_blue.png");
        std::fs::write(&path, &buf).unwrap();
        let color = calculate_avg_color(&path).unwrap();
        assert_eq!(color, "#0000ff");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_calculate_avg_color_white_rejected() {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 255, 255, 255]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();

        let path = std::env::temp_dir().join("test_favicon_white.png");
        std::fs::write(&path, &buf).unwrap();
        assert!(calculate_avg_color(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_calculate_avg_color_gray_rejected() {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([128, 128, 128, 255]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();

        let path = std::env::temp_dir().join("test_favicon_gray.png");
        std::fs::write(&path, &buf).unwrap();
        assert!(calculate_avg_color(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_calculate_avg_color_missing_file() {
        assert!(calculate_avg_color(Path::new("/nonexistent/icon.png")).is_none());
    }
}
