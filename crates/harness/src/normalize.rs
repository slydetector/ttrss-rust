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

/// Replace time/hex patterns in a string value.
pub fn normalize_string(s: &str) -> String {
    let mut out = s.to_string();
    out = RFC822_TS.replace_all(&out, "<ts>").into_owned();
    out = ISO_TS.replace_all(&out, "<ts>").into_owned();
    out = SLASH_TS.replace_all(&out, "<ts>").into_owned();
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

/// Normalize free text (HTML/XML/shell pages): same value patterns.
pub fn normalize_text(s: &str) -> String {
    normalize_string(s)
}
