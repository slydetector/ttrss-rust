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

/// `RSSUtils::FAVICON_ALLOWED_MIME_TYPES` (RSSUtils.php:9-15) — plus
/// `image/svg+xml`, a deliberate extension: the reference's list predates
/// SVG favicons, and its serving path never gated on MIME, so a
/// user-uploaded SVG icon already rendered in the PHP app.
const FAVICON_ALLOWED_MIME: &[&str] = &[
    "image/bmp",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/svg+xml",
    "image/vnd.microsoft.icon",
];

/// MIME gate for downloaded feed icons (see [`FAVICON_ALLOWED_MIME`]).
pub fn is_allowed_icon_mime(mime: &str) -> bool {
    FAVICON_ALLOWED_MIME.contains(&mime)
}

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
            let rel = attrs
                .get("rel")
                .map(|r| r.to_lowercase())
                .unwrap_or_default();
            if (rel == "shortcut icon" || rel == "icon" || rel == "alternate icon")
                && let Some(href) = attrs.get("href")
                && let Some(abs) = urlhelper::rewrite_relative(&base, href)
            {
                urls.push(abs);
            }
        }
    }

    // Fallback: `<base>/favicon.ico`.
    if urls.is_empty()
        && let Some(fallback) = urlhelper::rewrite_relative(&base, "/favicon.ico")
    {
        urls.push(fallback);
    }

    urls
}

/// The outcome of [`update_favicon`]. The feed-debug dialog logs it
/// (a deliberate extension beyond the reference — the PHP's
/// `RSSUtils::update_favicon` returns a bare bool and logs nothing).
#[derive(Debug)]
pub enum FaviconOutcome {
    /// The icon was saved.
    Saved { url: String, mime: String },
    /// The site page fetch/parse yielded no favicon candidates at all.
    NoCandidates,
    /// Candidates were found but none produced an allowed image type.
    /// One `(url, sniffed mime)` per attempt; `None` mime = the fetch
    /// itself failed.
    Rejected(Vec<(String, Option<String>)>),
    /// A usable icon was fetched but writing it to disk failed.
    WriteFailed(String),
}

/// `RSSUtils::update_favicon` (RSSUtils.php:1774-1822) — try each candidate
/// URL: fetch, sniff MIME, save to disk if allowed.
pub async fn update_favicon(cfg: &Config, site_url: &str, feed_id: i32) -> FaviconOutcome {
    let candidate_urls = get_favicon_urls(site_url).await;
    if candidate_urls.is_empty() {
        tracing::debug!(site_url, "favicon: no candidate URLs found");
        return FaviconOutcome::NoCandidates;
    }

    let mut rejected: Vec<(String, Option<String>)> = Vec::new();
    for url in &candidate_urls {
        let result = urlhelper::fetch(&FetchOptions {
            url: url.clone(),
            max_size: Some(cfg.max_favicon_file_size as u64),
            ..Default::default()
        })
        .await;

        let Some(body) = result.body else {
            rejected.push((url.clone(), None));
            continue;
        };

        let mime = crate::diskcache::sniff_mime(&body);
        if !is_allowed_icon_mime(mime) {
            rejected.push((url.clone(), Some(mime.to_string())));
            continue;
        }

        // Save to the feed-icon cache directory.
        let dir = &cfg.feed_icon_dir();
        if std::fs::create_dir_all(dir).is_err() {
            return FaviconOutcome::WriteFailed(url.clone());
        }
        let path = dir.join(feed_id.to_string());
        match std::fs::write(&path, &body) {
            Ok(_) => {
                tracing::debug!(feed_id, url, mime, "favicon: saved");
                return FaviconOutcome::Saved {
                    url: url.clone(),
                    mime: mime.to_string(),
                };
            }
            Err(e) => {
                tracing::warn!(feed_id, url, error = %e, "favicon: write failed");
                return FaviconOutcome::WriteFailed(url.clone());
            }
        }
    }

    FaviconOutcome::Rejected(rejected)
}

/// `Colors::calculate_avg_color` (colors.php:274-295) — decode the icon,
/// gather `(color, weight)` samples (every 4th pixel for raster formats,
/// weight 1; area-weighted fills for SVG), quantize each channel to 0x33
/// (51) steps, find the top-4 most frequent colors, and return the first
/// that passes the HSV threshold (s > 0.25, v > 0.25, not near-red, not
/// near-white).
/// Returns `None` if no suitable color is found.
pub fn calculate_avg_color(icon_path: &Path) -> Option<String> {
    let bytes = std::fs::read(icon_path).ok()?;

    // Quantized (color, weight) samples, in first-sight order: the order is
    // the tie-breaker for equal weights (PHP `arsort` is stable over
    // insertion order), so it must be preserved — a `HashMap`'s RandomState
    // iteration would not.
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    let mut order: Vec<([u8; 3], f64)> = Vec::new();
    let add = |counts: &mut std::collections::HashMap<[u8; 3], usize>,
               order: &mut Vec<([u8; 3], f64)>,
               rgb: [u8; 3],
               weight: f64| {
        let key = [
            ((f64::from(rgb[0]) / 51.0).round() * 51.0).min(255.0) as u8,
            ((f64::from(rgb[1]) / 51.0).round() * 51.0).min(255.0) as u8,
            ((f64::from(rgb[2]) / 51.0).round() * 51.0).min(255.0) as u8,
        ];
        match counts.get(&key) {
            Some(&i) => order[i].1 += weight,
            None => {
                counts.insert(key, order.len());
                order.push((key, weight));
            }
        }
    };

    if crate::diskcache::sniff_mime(&bytes) == "image/svg+xml" {
        // Deliberate extension: the reference's GD can't open SVG and such
        // icons stay at the default gray; we color them from the fill
        // palette (see svg_fill_samples).
        let samples = svg_fill_samples(&bytes)?;
        for (rgb, weight) in samples {
            add(&mut counts, &mut order, rgb, weight);
        }
    } else {
        let img = image::load_from_memory(&bytes).ok()?;
        let w = img.width();
        let h = img.height();
        if w == 0 || h == 0 {
            return None;
        }
        // Convert to RGBA for uniform pixel access.
        let rgba = img.to_rgba8();
        for x in (0..w).step_by(4) {
            for y in (0..h).step_by(4) {
                let [r, g, b, _] = rgba.get_pixel(x, y).0;
                add(&mut counts, &mut order, [r, g, b], 1.0);
            }
        }
    }

    // Sort by weight desc, take top 4 — stable, so exact ties keep
    // first-sight order (PHP `arsort`).
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let palette: Vec<[u8; 3]> = order.iter().take(4).map(|&(c, _)| c).collect();

    // For each color in the palette, convert to HSV and check the threshold.
    for [r, g, b] in &palette {
        let (hue, s, v) = rgb_to_hsv(*r, *g, *b);
        if s > 0.25 && v > 0.25 && !(hue < 0.01 && s < 0.01) && !(hue < 0.01 && v > 0.99) {
            return Some(format!("#{r:02x}{g:02x}{b:02x}"));
        }
    }

    None
}

/// SVG branch for [`calculate_avg_color`]: collect each fill's color
/// weighted by bounding-box area × cumulative opacity, walking the resolved
/// [`usvg::Tree`] — **no rasterization** (the dep is usvg alone, parse +
/// tree, so a huge viewBox can't blow up memory the way a rasterizer
/// pixmap could).
///
/// Deliberately conservative: only `fill` paint is considered — strokes,
/// `image`, `pattern` paint, masks and filters are ignored; gradients are
/// approximated by the plain mean of their stops. An icon with no usable
/// solid fills degrades to the default gray, exactly what the reference's
/// GD does for every SVG.
fn svg_fill_samples(bytes: &[u8]) -> Option<Vec<([u8; 3], f64)>> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let mut out: Vec<([u8; 3], f64)> = Vec::new();

    // Explicit stack of (node, cumulative opacity) — no recursion.
    let mut stack: Vec<(&usvg::Node, f64)> =
        tree.root().children().iter().map(|n| (n, 1.0)).collect();

    while let Some((node, opacity)) = stack.pop() {
        match node {
            usvg::Node::Group(g) => {
                let op = opacity * g.opacity().get() as f64;
                for child in g.children() {
                    stack.push((child, op));
                }
            }
            usvg::Node::Path(p) => {
                let Some(fill) = p.fill() else {
                    continue;
                };
                let bb = p.abs_bounding_box();
                let area = bb.width() as f64 * bb.height() as f64;
                let weight = area * opacity * fill.opacity().get() as f64;
                if weight <= 0.0 {
                    continue;
                }
                let color = match fill.paint() {
                    usvg::Paint::Color(c) => Some([c.red, c.green, c.blue]),
                    usvg::Paint::LinearGradient(g) => gradient_mean(g.stops()),
                    usvg::Paint::RadialGradient(g) => gradient_mean(g.stops()),
                    // Pattern paint: no cheap color; skip.
                    usvg::Paint::Pattern(_) => None,
                };
                if let Some(rgb) = color {
                    out.push((rgb, weight));
                }
            }
            usvg::Node::Image(_) | usvg::Node::Text(_) => {}
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Gradient → plain mean of its stop colors (a deliberate approximation:
/// a favicon's 4-digit accent doesn't need offset-weighted sampling).
fn gradient_mean(stops: &[usvg::Stop]) -> Option<[u8; 3]> {
    if stops.is_empty() {
        return None;
    }
    let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
    for s in stops {
        let c = s.color();
        r += c.red as f64;
        g += c.green as f64;
        b += c.blue as f64;
    }
    let n = stops.len() as f64;
    Some([
        (r / n).round() as u8,
        (g / n).round() as u8,
        (b / n).round() as u8,
    ])
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
    fn test_calculate_avg_color_tie_is_deterministic_first_sight() {
        // Left half blue / right half green → an exact count tie (8:8 in the
        // step-4 scan). PHP `arsort` is stable over the x-outer scan's
        // first-sight order → the (0,0) color wins, and the result must be
        // the same on every call (a `HashMap` RandomState iteration used to
        // make the tie-break nondeterministic).
        let mut img = image::RgbaImage::new(16, 16);
        for x in 0..16u32 {
            for y in 0..16u32 {
                img.put_pixel(
                    x,
                    y,
                    if x < 8 {
                        image::Rgba([0, 0, 255, 255])
                    } else {
                        image::Rgba([0, 255, 0, 255])
                    },
                );
            }
        }
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();

        let path = std::env::temp_dir().join("test_favicon_tie.png");
        std::fs::write(&path, &buf).unwrap();
        for _ in 0..10 {
            assert_eq!(calculate_avg_color(&path).unwrap(), "#0000ff");
        }
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

    fn write_svg(name: &str, markup: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, markup).unwrap();
        path
    }

    #[test]
    fn test_is_allowed_icon_mime() {
        assert!(is_allowed_icon_mime("image/svg+xml"));
        assert!(is_allowed_icon_mime("image/png"));
        assert!(!is_allowed_icon_mime("text/html"));
        assert!(!is_allowed_icon_mime("application/xml"));
    }

    #[test]
    fn test_calculate_avg_color_solid_svg() {
        let path = write_svg(
            "test_favicon_svg_solid.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#0000ff\"/></svg>",
        );
        assert_eq!(calculate_avg_color(&path).unwrap(), "#0000ff");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_calculate_avg_color_svg_gradient() {
        // Two-stop blue→green gradient: the stop mean (0,128,128) quantizes
        // to the 153 bucket → #009999.
        let path = write_svg(
            "test_favicon_svg_grad.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><defs><linearGradient id=\"g\"><stop offset=\"0\" stop-color=\"#0000ff\"/><stop offset=\"1\" stop-color=\"#00ff00\"/></linearGradient></defs><rect width=\"16\" height=\"16\" fill=\"url(#g)\"/></svg>",
        );
        assert_eq!(calculate_avg_color(&path).unwrap(), "#009999");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_calculate_avg_color_svg_stroke_only_rejected() {
        // No fill paint (strokes are ignored by design) → no usable color,
        // the icon degrades to the default gray like the reference's GD.
        let path = write_svg(
            "test_favicon_svg_stroke.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect x=\"2\" y=\"2\" width=\"12\" height=\"12\" fill=\"none\" stroke=\"#ff0000\"/></svg>",
        );
        assert!(calculate_avg_color(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }
}
