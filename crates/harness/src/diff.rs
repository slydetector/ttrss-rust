//! Structural comparison of two run directories.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::html;
use crate::normalize;

#[derive(Clone, Deserialize)]
struct Exchange {
    method: String,
    url: String,
    body_encoding: String,
    status: u16,
    headers: Vec<(String, String)>,
    body_text: Option<String>,
    body_b64: Option<String>,
    #[serde(default)]
    db_state: Option<serde_json::Value>,
}

/// Response headers that legitimately differ per run/environment. Transport
/// headers (nginx vs the Rust server), per-request values (date/etag), and
/// server fingerprints (server / x-powered-by) are all ignored — we compare the
/// semantically meaningful headers (content-type, cache-control, set-cookie,
/// location, ...).
const VOLATILE_HEADERS: &[&str] = &[
    "date",
    "etag",
    "last-modified",
    "content-length",
    "api-content-length",
    "server",
    "x-powered-by",
    "x-request-id",
    // transport / connection framing (nginx + PHP-FPM vs axum)
    "connection",
    "transfer-encoding",
    "vary",
    "accept-ranges",
];

fn filtered_headers(headers: &[(String, String)]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (k, v) in headers {
        let lk = k.to_ascii_lowercase();
        if VOLATILE_HEADERS.contains(&lk.as_str()) {
            continue;
        }
        // All header values are value-normalized like body text: set-cookie
        // carries the session id + expires timestamp, and `location` (and any
        // URL-bearing header) carries the run's host:port, which differ per
        // run/environment by construction.
        let v = normalize::normalize_string(v);
        // keep first value per header
        map.entry(lk).or_insert_with(|| v);
    }
    map
}

pub fn diff(a: &Path, b: &Path, max_details: usize, allow_mismatches: usize) -> Result<()> {
    let files = |dir: &Path| -> Result<Vec<String>> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".json"))
            .collect();
        v.sort();
        Ok(v)
    };

    let fa = files(a)?;
    let fb = files(b)?;
    let only_a: Vec<&String> = fa.iter().filter(|n| !fb.contains(n)).collect();
    let only_b: Vec<&String> = fb.iter().filter(|n| !fa.contains(n)).collect();
    for n in &only_a {
        println!("[diff] only in A: {n}");
    }
    for n in &only_b {
        println!("[diff] only in B: {n}");
    }

    let mut mismatches = 0usize;
    for name in fa.iter().filter(|n| fb.contains(n)) {
        let ea: Exchange = serde_json::from_str(
            &std::fs::read_to_string(a.join(name))
                .with_context(|| format!("reading {}/{}", a.display(), name))?,
        )
        .with_context(|| format!("parsing {}/{}", a.display(), name))?;
        let eb: Exchange = serde_json::from_str(
            &std::fs::read_to_string(b.join(name))
                .with_context(|| format!("reading {}/{}", b.display(), name))?,
        )
        .with_context(|| format!("parsing {}/{}", b.display(), name))?;

        let details = compare_exchanges(&ea, &eb);
        if details.is_empty() {
            println!("[diff] ok      {name}");
        } else {
            mismatches += 1;
            println!("[diff] MISMATCH {name} ({} detail(s))", details.len());
            for d in details.iter().take(max_details) {
                println!("            {d}");
            }
            if details.len() > max_details {
                println!("            ... ({} more)", details.len() - max_details);
            }
        }
    }

    let total = fa.len().max(fb.len());
    println!(
        "[diff] {total} exchange(s): {} match, {} mismatch, {} only-A, {} only-B",
        total.saturating_sub(mismatches).saturating_sub(only_a.len()).saturating_sub(only_b.len()),
        mismatches,
        only_a.len(),
        only_b.len()
    );

    if mismatches + only_a.len() + only_b.len() > allow_mismatches {
        anyhow::bail!(
            "mismatches beyond tolerance ({mismatches} + {}/{} one-sided)",
            only_a.len(),
            only_b.len()
        );
    }
    Ok(())
}

/// Extract the path (+ query) from a URL, stripping the scheme and authority,
/// so two runs on different host:port compare as the same request.
fn req_path(url: &str) -> &str {
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        return rest.find('/').map(|i| &rest[i..]).unwrap_or("");
    }
    url
}

fn compare_exchanges(ea: &Exchange, eb: &Exchange) -> Vec<String> {
    let mut details = Vec::new();
    if ea.status != eb.status {
        details.push(format!("status: {} vs {}", ea.status, eb.status));
    }
    // Compare the request method + path, not the full URL: the oracle and the
    // Rust server run on different host:port, so the authority legitimately
    // differs while the path/method/body must match.
    if ea.method != eb.method || req_path(&ea.url) != req_path(&eb.url) {
        details.push(format!(
            "request: {} {} vs {} {}",
            ea.method,
            req_path(&ea.url),
            eb.method,
            req_path(&eb.url)
        ));
    }
    let ha = filtered_headers(&ea.headers);
    let hb = filtered_headers(&eb.headers);
    for k in ha.keys().chain(hb.keys()) {
        match (ha.get(k), hb.get(k)) {
            (Some(a), Some(b)) if a == b => {}
            (a, b) => details.push(format!(
                "header {}: {:?} vs {:?}",
                k,
                a.map(|s| snippet(s)),
                b.map(|s| snippet(s))
            )),
        }
    }
    match (ea.body_encoding.as_str(), eb.body_encoding.as_str()) {
        ("text", "text") => {
            let a = ea.body_text.clone().unwrap_or_default();
            let b = eb.body_text.clone().unwrap_or_default();
            let ja = normalize::normalize_json(&parse_or_raw(&a));
            let jb = normalize::normalize_json(&parse_or_raw(&b));
            if is_json(&a) && is_json(&b) {
                let mut paths = Vec::new();
                diff_json(&ja, &jb, "$", &mut paths);
                if paths.is_empty() {
                    // JSON-equal after normalization but raw text differs
                    // (key order / whitespace only) — not a meaningful diff.
                } else {
                    for p in paths {
                        details.push(p);
                    }
                }
            } else {
                // HTML bodies are compared as canonical DOM trees (structure +
                // meaningful values); other text falls back to value
                // normalization.
                let is_html = html::is_html(&a) && html::is_html(&b);
                let (na, nb, label) = if is_html {
                    (html::canonical_html(&a), html::canonical_html(&b), "html")
                } else {
                    (
                        normalize::normalize_text(&a),
                        normalize::normalize_text(&b),
                        "text",
                    )
                };
                if na != nb {
                    if let Some(off) = first_diff_offset(&na, &nb) {
                        details.push(format!(
                            "{label} body differs @{}: {:?} vs {:?}",
                            off,
                            ctx(&na, off),
                            ctx(&nb, off)
                        ));
                    }
                }
            }
        }
        ("binary", "binary") => {
            if ea.body_b64 != eb.body_b64 {
                details.push(format!(
                    "binary body differs (len {} vs {})",
                    ea.body_b64.as_deref().map(|s| s.len()).unwrap_or(0),
                    eb.body_b64.as_deref().map(|s| s.len()).unwrap_or(0)
                ));
            }
        }
        (x, y) => details.push(format!("body encoding mismatch: {x} vs {y}")),
    }
    // Compare the resulting DB state (M3 mutations), if both runs captured it.
    match (&ea.db_state, &eb.db_state) {
        (Some(a), Some(b)) => {
            let mut paths = Vec::new();
            diff_json(a, b, "$.db_state", &mut paths);
            details.extend(paths);
        }
        (Some(_), None) => details.push("db_state: present vs <absent>".into()),
        (None, Some(_)) => details.push("db_state: <absent> vs present".into()),
        (None, None) => {}
    }
    details
}

fn is_json(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

fn parse_or_raw(s: &str) -> serde_json::Value {
    if is_json(s) {
        if let Ok(v) = serde_json::from_str(s) {
            return v;
        }
    }
    serde_json::Value::String(s.to_string())
}

fn diff_json(a: &serde_json::Value, b: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    use serde_json::Value::*;
    match (a, b) {
        (Object(ma), Object(mb)) => {
            for k in ma.keys().chain(mb.keys()) {
                match (ma.get(k), mb.get(k)) {
                    (Some(x), Some(y)) => diff_json(x, y, &format!("{path}.{k}"), out),
                    (Some(x), None) => out.push(format!("{path}.{k}: {} vs <absent>", summary(x))),
                    (None, Some(y)) => out.push(format!("{path}.{k}: <absent> vs {}", summary(y))),
                    (None, None) => {}
                }
            }
        }
        (Array(va), Array(vb)) => {
            if va.len() != vb.len() {
                out.push(format!("{path}: array len {} vs {}", va.len(), vb.len()));
            }
            for (i, (x, y)) in va.iter().zip(vb.iter()).enumerate() {
                diff_json(x, y, &format!("{path}[{i}]"), out);
            }
        }
        (x, y) if x == y => {}
        (x, y) => out.push(format!("{path}: {} vs {}", summary(x), summary(y))),
    }
}

fn summary(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..80.min(s.len())])
    } else {
        s
    }
}

fn snippet(s: &str) -> String {
    if s.len() > 60 {
        format!("{}…", &s[..60.min(s.len())])
    } else {
        s.to_string()
    }
}

fn first_diff_offset(a: &str, b: &str) -> Option<usize> {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    for (i, (ca, cb)) in ab.iter().zip(bb.iter()).enumerate() {
        if ca != cb {
            return Some(i);
        }
    }
    if ab.len() != bb.len() {
        Some(ab.len().min(bb.len()))
    } else {
        None
    }
}

fn ctx(s: &str, off: usize) -> String {
    if s.is_empty() {
        return "<empty>".to_string();
    }
    let upto = off.max(20).min(s.len());
    let start = s[..upto].rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let end = (start + 60).min(s.len());
    format!("…{}…", s[start..end].replace('\n', "\\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(status: u16, body: &str) -> Exchange {
        Exchange {
            method: "GET".into(),
            url: "http://x/".into(),
            body_encoding: "text".into(),
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body_text: Some(body.to_string()),
            body_b64: None,
            db_state: None,
        }
    }

    #[test]
    fn equal_exchanges_no_details() {
        let a = exchange(200, "{\"v\":1}");
        let b = exchange(200, "{\"v\": 1}");
        assert!(compare_exchanges(&a, &b).is_empty());
    }

    #[test]
    fn status_diff_detected() {
        let a = exchange(200, "{}");
        let b = exchange(500, "");
        let d = compare_exchanges(&a, &b);
        assert!(d.iter().any(|s| s.starts_with("status")));
    }

    #[test]
    fn volatile_values_ignored() {
        let a = exchange(200, "{\"csrf_token\":\"aaa\",\"session_id\":\"bbb\",\"n\":5}");
        let b = exchange(200, "{\"csrf_token\":\"ccc\",\"session_id\":\"ddd\",\"n\":5}");
        assert!(compare_exchanges(&a, &b).is_empty());
    }

    #[test]
    fn data_diff_detected() {
        let a = exchange(200, "{\"count\":1}");
        let b = exchange(200, "{\"count\":2}");
        let d = compare_exchanges(&a, &b);
        assert!(d.iter().any(|s| s.contains("$.count")));
    }

    #[test]
    fn timestamp_and_cookie_normalization() {
        let mut a = exchange(200, "{}");
        a.headers = vec![(
            "set-cookie".into(),
            "ttrss_sid=4bc3e91e9f36e52e87be607fad96d384; expires=Fri, 04 Sep 2026 21:59:39 GMT".into(),
        )];
        let mut b = a.clone();
        b.headers = vec![(
            "set-cookie".into(),
            "ttrss_sid=c950b757057d8622ce8160c9317ed271; expires=Fri, 04 Sep 2026 22:02:31 GMT".into(),
        )];
        assert!(compare_exchanges(&a, &b).is_empty());
    }

    #[test]
    fn empty_body_ctx_does_not_panic() {
        let a = exchange(200, "");
        let b = exchange(200, "x");
        let _ = compare_exchanges(&a, &b);
    }
}
