//! Non-determinism normalization for response comparison.
//!
//! Two runs of the same scenario suite against two servers (or two runs
//! minutes apart) must compare equal *modulo* values that legitimately
//! differ: CSRF tokens, session ids, timestamps derived from "now", and
//! long random hex strings. Data values (ids, titles, counters) must NOT be
//! normalized away — those are what we're verifying.

use serde_json::Value;
use std::sync::LazyLock;

use crate::html;

/// JSON keys whose values are inherently per-run and not comparable.
pub const VOLATILE_JSON_KEYS: &[&str] = &[
    "csrf_token",
    "session_id",
    "sid",
    // server-side counters that grow with request history, not comparable
    // across runs/servers
    "recent_log_events",
    // update-daemon state (the Rust server emits these stubbed, D6 = c) —
    // environment/volatile (the stamp is a moving timestamp), not app behavior
    "daemon_is_running",
    "daemon_stamp",
    "daemon_stamp_ok",
    // runtime environment facts (the oracle reports PHP's; the Rust server is
    // not PHP). Unused by the frontend.
    "php_platform",
    "php_version",
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

/// PHP slash dates as used in digests: `date("Y/m/d")` + `date("G:i")` (e.g.
/// "2026-09-07 3:16") or a full `Y/m/d H:i:s`. The time part and the
/// leading zero on the hour are optional. The slash also matches the
/// JSON-escaped form (`\\/`), because the PHP `json_encode` response body
/// escapes `/` while the Rust `serde_json` body does not — so the two sides
/// reach the normalizer in different forms.
static SLASH_TS: LazyLock<regex::Regex> =
    LazyLock::new(||
        regex::Regex::new(r"\d{4}\\?/\d{2}\\?/\d{2}(?: \d{1,2}:\d{2}(?::\d{2})?)?").unwrap());

/// Time-of-day only (PHP date("H:i")), e.g. "last login: 21:59".
static TIME_ONLY: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b\d{1,2}:\d{2}\b").unwrap());

/// tt-rss "smart dates" (TimeHelper::smart_date_time / make_local_datetime):
/// today renders as a bare time ("13:28"), other dates carry a day+month
/// prefix ("1 Jan, 12:00", "Thu, 1 Jan 2026", "1 Jan 2025, 12:00") per the
/// user's SHORT/LONG_DATE_FORMAT prefs. These are display renderings of
/// stored datetimes that legitimately differ between runs (corpus logins and
/// updates re-stamp them) and between servers, so collapse the whole smart
/// date — date prefix and/or time — to a single token. Applied after the
/// ISO/RFC822/slash timestamp rules (which already consume their formats) and
/// before TIME_ONLY (bare time-of-day), so "1 Jan, 12:00" and "13:28" both
/// normalize identically.
static SMART_DATE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?:[A-Z][a-z]{2}, )?\d{1,2} (?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)(?: \d{4})?,?(?: \d{1,2}:\d{2}(?: [ap]m)?)?",
    )
    .unwrap()
});

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

/// URL authority with an explicit port, `://host:port` (e.g. in a `Location`
/// header or a body URL). The oracle (8280) and the Rust server (8281) are the
/// same application on different ports by design, so the port is environment,
/// not data: strip it so the two runs compare. Portless URLs (feed links,
/// `https://arstechnica.com/...`) are untouched, as are bare numbers. Both the
/// literal form and the percent-encoded form (form `action`s carry
/// `http%3A%2F%2F…%3A8280…`) are handled.
static URL_PORT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"://([^:/\s]+):\d+").unwrap());
static URL_PORT_ENC: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)%3A[0-9]+").unwrap());

/// Replace time/hex/cache-bust patterns in a string value.
pub fn normalize_string(s: &str) -> String {
    let mut out = s.to_string();
    out = RFC822_TS.replace_all(&out, "<ts>").into_owned();
    out = ISO_TS.replace_all(&out, "<ts>").into_owned();
    out = SLASH_TS.replace_all(&out, "<ts>").into_owned();
    // URL authority ports (response `Location`, form actions, body URLs):
    // oracle vs Rust run on different ports by construction.
    out = URL_PORT.replace_all(&out, "://$1").into_owned();
    out = URL_PORT_ENC.replace_all(&out, "").into_owned();
    out = CACHE_BUST.replace_all(&out, "?").into_owned();
    out = UNIX_TS.replace_all(&out, "<ts>").into_owned();
    out = HEX64.replace_all(&out, "<hex64>").into_owned();
    out = SHA1.replace_all(&out, "<sha1>").into_owned();
    out = HEX32.replace_all(&out, "<hex32>").into_owned();
    // time-of-day last: it would otherwise re-match inside timestamps.
    // Smart dates and bare times normalize to the same token: both are
    // "the displayed value of a stored datetime", which is not comparable
    // across runs (a "today" time vs a "D Mon, time" render of an older one
    // must not diff).
    out = SMART_DATE.replace_all(&out, "<ts>").into_owned();
    out = TIME_ONLY.replace_all(&out, "<ts>").into_owned();
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
                // The counter/feed `ts` field is the icon-cache file mtime
                // (Cache_Local::get_mtime of the feed-icon). It is a "when was
                // this favicon last fetched" cache timestamp, not app data: the
                // oracle stamps it to the restore time (docker cp) while the
                // port reads the seed-time mtime of the committed icon files, so
                // the absolute value legitimately differs. Normalize a positive
                // epoch to a token; `ts: 0` (no icon cached) is meaningful and
                // left as-is so a has-icon vs no-icon difference still diffs.
                if k == "ts"
                    && let Value::Number(n) = v
                        && let Some(i) = n.as_u64()
                            && i >= 1_000_000_000 {
                                out.insert(k.clone(), Value::String("<ts>".into()));
                                continue;
                            }
                out.insert(k.clone(), normalize_json(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_json).collect()),
        Value::String(s) => {
            // HTML-bearing string fields (article body, plugin buttons, ...) are
            // compared *structurally* — parsed into a canonical DOM — so entity
            // spelling and inter-tag whitespace don't matter. We own the render,
            // not the source: the feed origin is canonical for the content, so
            // byte-parity is not required. Non-HTML strings stay value-normalized.
            Value::String(if html::is_html_fragment(s) {
                html::canonical_html(s)
            } else {
                normalize_string(s)
            })
        }
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
    fn url_authority_port_stripped() {
        // literal URLs (Location headers)
        assert_eq!(
            normalize_string("http://127.0.0.1:8280/tt-rss/index.php"),
            "http://127.0.0.1/tt-rss/index.php"
        );
        assert_eq!(
            normalize_string("http://127.0.0.1:8281/tt-rss/index.php"),
            normalize_string("http://127.0.0.1:8280/tt-rss/index.php")
        );
        // percent-encoded URLs (form actions)
        assert_eq!(
            normalize_string("public.php?return=http%3A%2F%2F127.0.0.1%3A8280%2Ftt-rss%2F"),
            normalize_string("public.php?return=http%3A%2F%2F127.0.0.1%3A8281%2Ftt-rss%2F")
        );
        // portless URLs are untouched
        assert_eq!(
            normalize_string("https://arstechnica.com/feed/"),
            "https://arstechnica.com/feed/"
        );
        // a bare number is not a port
        assert_eq!(normalize_string("port 8280"), "port 8280");
    }

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
    fn slash_dates_normalized_plain_and_json_escaped() {
        // Plain form (Rust `serde_json` body).
        assert_eq!(normalize_string("as of 2026/09/07"), "as of <ts>");
        assert_eq!(normalize_string("as of 2026/09/07 3:16"), "as of <ts>");
        assert_eq!(normalize_string("as of 2026/09/07 15:24:05"), "as of <ts>");
        // JSON-escaped form (PHP `json_encode` body escapes `/` as `\/`).
        assert_eq!(normalize_string(r"as of 2026\/09\/07"), "as of <ts>");
        assert_eq!(normalize_string(r"as of 2026\/09\/07 3:16"), "as of <ts>");
        // Both sides normalize to the same token.
        assert_eq!(
            normalize_string("as of 2026/09/07 3:16"),
            normalize_string(r"as of 2026\/09\/07 15:24")
        );
    }

    #[test]
    fn smart_dates_normalized_like_bare_times() {
        // tt-rss smart-date renders of the same underlying stored datetime
        // must normalize to one token, whatever the date-format prefs.
        assert_eq!(normalize_string("13:28"), "<ts>");
        assert_eq!(normalize_string("1 Jan, 12:00"), "<ts>");
        assert_eq!(normalize_string("1 Jan 2025, 12:00"), "<ts>");
        assert_eq!(normalize_string("Thu, 1 Jan 2026"), "<ts>");
        assert_eq!(normalize_string("1 Jan"), "<ts>");
        // a bare time and a prefixed smart date of another date are equal
        assert_eq!(
            normalize_string("last login 13:28"),
            normalize_string("last login 1 Jan, 12:00")
        );
        // data that merely contains the pattern is left alone
        assert_eq!(normalize_string("42 items"), "42 items");
        // RFC822 is handled by its own (earlier) rule, not half-matched here
        assert_eq!(
            normalize_string("Fri, 04 Sep 2026 21:59:39 GMT"),
            "<ts>"
        );
    }

    #[test]
    fn html_fields_in_json_compared_structurally() {
        // Same article body, different entity spelling + inter-tag whitespace:
        // must normalize to the same value (structural compare).
        let a = serde_json::json!({"content": "<p>It&rsquo;s a <b>test</b> &amp; more</p>", "title": "Hi"});
        let b = serde_json::json!({"content": "<p>It&#8217;s a\n  <b>test</b> &amp; more</p>", "title": "Hi"});
        assert_eq!(normalize_json(&a), normalize_json(&b));
        // A real structural difference is still detected.
        let c = serde_json::json!({"content": "<p>It&rsquo;s a <i>test</i> &amp; more</p>"});
        assert_ne!(normalize_json(&a), normalize_json(&c));
        // A non-HTML string field is value-normalized, not structural.
        let d = serde_json::json!({"content_preview": "&mdash; text &hellip;"});
        let e = serde_json::json!({"content_preview": "&mdash; text&hellip;"});
        assert_ne!(normalize_json(&d), normalize_json(&e));
    }

    #[test]
    fn icon_cache_ts_normalized() {
        // Positive epoch icon mtimes (which differ: oracle=restore time, port
        // =seed time) collapse to one token...
        let a = serde_json::json!({"counters":[{"id":29,"ts":1788702740}]});
        let b = serde_json::json!({"counters":[{"id":29,"ts":1788525223}]});
        assert_eq!(normalize_json(&a), normalize_json(&b));
        // ...but a no-icon feed (`ts:0`) is data, not a timestamp, and a real
        // has-icon vs no-icon difference is still detected.
        let c = serde_json::json!({"counters":[{"id":29,"ts":0}]});
        assert_ne!(normalize_json(&a), normalize_json(&c));
        assert_eq!(normalize_json(&c), normalize_json(&serde_json::json!({"counters":[{"id":29,"ts":0}]})));
        // non-epoch `ts` values (e.g. small ints) are left alone
        let d = serde_json::json!({"ts":0});
        assert_eq!(normalize_json(&d), serde_json::json!({"ts":0}));
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
