//! Non-determinism normalization for response comparison.
//!
//! Two runs of the same scenario suite against two servers (or two runs
//! minutes apart) must compare equal *modulo* values that legitimately
//! differ: CSRF tokens, session ids, timestamps derived from "now", and
//! long random hex strings. Data values (ids, titles, counters) must NOT be
//! normalized away — those are what we're verifying.

use serde_json::Value;
use std::sync::LazyLock;

/// JSON keys whose values are inherently per-run and not comparable.
pub const VOLATILE_JSON_KEYS: &[&str] = &[
    "csrf_token",
    "session_id",
    "sid",
    // server-side counters that grow with request history, not comparable
    // across runs/servers
    "recent_log_events",
];

static ISO_TS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2})?(Z|[+-]\d{2}:?\d{2})?").unwrap()
});

static RFC822_TS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"[A-Z][a-z]{2}, \d{1,2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} (?:[+-]\d{4}|GMT)",
    )
    .unwrap()
});

/// PHP date("Y/m/d H:i") as used in digests ("2026-09-03 21:59").
static SLASH_TS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}").unwrap());

/// Time-of-day only (PHP date("H:i")), e.g. "last login: 21:59".
static TIME_ONLY: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b\d{1,2}:\d{2}\b").unwrap());

static HEX64: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[0-9a-f]{64}").unwrap());
static SHA1: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b[0-9a-f]{40}\b").unwrap());
static HEX32: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b[0-9a-f]{32}\b").unwrap());

/// Query string that is a bare timestamp cache-buster: `?1788113929` (6+ digits
/// immediately after `?`). The exact value is file-mtime / "now"-dependent and
/// legitimately differs between the oracle and the port.
static CACHE_BUST: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\?\d{6,}").unwrap());

/// A bare 10-digit unix timestamp (e.g. the dojoConfig `cacheBust: "1788113929"`
/// in the shell, or a feed-publish epoch in API JSON). Current epochs are ~10
/// digits starting with `1`; the `\b` boundaries keep it from matching inside a
/// longer digit run (digits are word chars), so an all-digit 32-hex value is
/// left to the hex normalizers. File-mtime / "now" dependent, so it
/// legitimately differs between the oracle and the port.
static UNIX_TS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b1\d{9}\b").unwrap());

/// Whitespace that exists only between two tags (i.e. `>  ...  <` with nothing
/// but whitespace inside). Collapsing it compares HTML structure while leaving
/// real text content (e.g. `>Use less traffic<`) untouched.
static INTER_TAG_WS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r">\s+<").unwrap());

/// Replace time/hex/cache-bust patterns in a string value.
pub fn normalize_string(s: &str) -> String {
    let mut out = s.to_string();
    out = RFC822_TS.replace_all(&out, "<ts>").into_owned();
    out = ISO_TS.replace_all(&out, "<ts>").into_owned();
    out = SLASH_TS.replace_all(&out, "<ts>").into_owned();
    out = CACHE_BUST.replace_all(&out, "?").into_owned();
    out = UNIX_TS.replace_all(&out, "<ts>").into_owned();
    out = HEX64.replace_all(&out, "<hex64>").into_owned();
    out = SHA1.replace_all(&out, "<sha1>").into_owned();
    out = HEX32.replace_all(&out, "<hex32>").into_owned();
    // time-of-day last: it would otherwise re-match inside timestamps
    out = TIME_ONLY.replace_all(&out, "<time>").into_owned();
    out
}

/// Recursively normalize a JSON value: drop volatile keys, normalize strings.
pub fn normalize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if VOLATILE_JSON_KEYS.contains(&k.as_str()) {
                    continue;
                }
                out.insert(k.clone(), normalize_json(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_json).collect()),
        Value::String(s) => Value::String(normalize_string(s)),
        other => other.clone(),
    }
}

/// Normalize free text (HTML/XML/shell pages): value patterns, plus inter-tag
/// whitespace collapsing and trimming so we compare HTML *structure* + dynamic
/// values rather than raw bytes. Whitespace that PHP leaves behind from
/// `<?php if (...) ?>` blocks is functionally irrelevant and is collapsed.
pub fn normalize_text(s: &str) -> String {
    let s = normalize_string(s);
    // Match the full `> ... <` span (both brackets included) and collapse it
    // to `><`, dropping only the whitespace between the tags.
    let s = INTER_TAG_WS.replace_all(&s, "><").into_owned();
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_bust_normalized() {
        assert_eq!(normalize_string("js/common.js?1788113929"), "js/common.js?");
        assert_eq!(normalize_string("themes/light.css?1788113929"), "themes/light.css?");
        // short query params are NOT cache-busters and are left alone
        assert_eq!(normalize_string("public.php?op=login"), "public.php?op=login");
    }

    #[test]
    fn inter_tag_whitespace_collapsed() {
        let a = "<div>\n\t\t<label>Use less traffic</label>\n</div>";
        let b = "<div><label>Use less traffic</label></div>";
        assert_eq!(normalize_text(a), normalize_text(b));
        // text content is preserved (not whitespace)
        assert!(normalize_text(a).contains("Use less traffic"));
    }

    #[test]
    fn html_differs_only_in_cache_bust_and_ws() {
        let oracle =
            "<html>\n<head>\n<script src=\"js/common.js?1788113929\"></script>\n</head>\n</html>";
        let mine = "<html><head><script src=\"js/common.js?1789000000\"></script></head></html>";
        assert_eq!(normalize_text(oracle), normalize_text(mine));
    }

    #[test]
    fn html_real_difference_still_detected() {
        let a = "<div id=\"feedTree\"></div>";
        let b = "<div id=\"feed-tree\"></div>";
        assert_ne!(normalize_text(a), normalize_text(b));
    }

    #[test]
    fn csrf_hex_still_normalized_in_text() {
        let a = "const __csrf_token = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";";
        let b = "const __csrf_token = \"ffffffffffffffffffffffffffffffff\";";
        assert_eq!(normalize_text(a), normalize_text(b));
    }

    #[test]
    fn bare_unix_timestamp_normalized() {
        assert_eq!(normalize_string("1788113929"), "<ts>");
        assert_eq!(normalize_string("cacheBust: 1788113929"), "cacheBust: <ts>");
        // a longer digit run has no boundary, so it's left alone
        assert_eq!(normalize_string("12345678901234567890"), "12345678901234567890");
        // small ids are untouched
        assert_eq!(normalize_string("12345"), "12345");
    }
}
