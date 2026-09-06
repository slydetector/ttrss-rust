//! Ports of PHP built-ins shared across server modules.
//!
//! The PHP reference calls the single global `strip_tags()` from many places
//! (sanitizer's `clean()`, `Feeds` view params, `Prefs::_set`, …); the Rust
//! port lives here once and is reused by every module.

use rand::Rng;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// PHP built-in `strip_tags`: drop HTML/XML tags and comments, keep text
/// (entity references are left as-is).
///
/// Faithful to the built-in: a tag starts with `<` followed by a letter,
/// `/`, `!`, or `?`; an unterminated tag strips to the end of the string;
/// `<!-- … -->` comments are removed; a `<` that does not start a tag is
/// kept.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(c) = after.chars().next() {
            if c == '!' && after.starts_with("<!--") {
                match after[4..].find("-->") {
                    Some(j) => rest = &after[4 + j + 3..],
                    None => return out,
                }
                continue;
            }
            if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?' {
                match after.find('>') {
                    Some(j) => {
                        rest = &after[j + 1..];
                        continue;
                    }
                    // non-terminated tag: PHP strips to the end of the string
                    None => return out,
                }
            }
        }
        out.push('<');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// PHP built-in `trim()` (default character list ` "\t\n\r\0\x0B`).
///
/// Unlike `str::trim`, does *not* strip Unicode whitespace such as `\u{A0}`.
pub fn php_trim(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B'))
}

/// PHP `clean()` (`include/functions.php`): strip tags + trim every string in the
/// value, recursively (PHP `array_map` over list *and* map arrays, keys kept).
/// Non-string scalars (numbers, booleans, null) pass through unchanged.
pub fn clean(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.iter().map(clean).collect()),
        Value::Object(map) => Value::Object(
            map.iter().map(|(k, val)| (k.clone(), clean(val))).collect(),
        ),
        Value::String(s) => Value::String(clean_str(s)),
        other => other.clone(),
    }
}

/// `clean()` for a bare string — the form most call sites use.
pub fn clean_str(s: &str) -> String {
    php_trim(&strip_tags(s)).to_string()
}

/// PHP `mb_substr_replace()` (`include/functions.php`): character-wise
/// (UTF-8) replacement of `[position, position+length)` with `replacement`.
pub fn mb_substr_replace(original: &str, replacement: &str, position: usize, length: usize) -> String {
    let chars: Vec<char> = original.chars().collect();
    let start: String = chars.iter().take(position).collect();
    let end: String = chars.iter().skip(position.saturating_add(length)).collect();
    format!("{start}{replacement}{end}")
}

/// PHP `truncate_middle($str, $max_len, $suffix = '&hellip;')`:
/// when the char count exceeds `$max_len`, replace the middle
/// `($len - $max_len)` chars (centered at `$max_len / 2`) with the suffix.
pub fn truncate_middle(s: &str, max_len: usize, suffix: &str) -> String {
    let len = s.chars().count();
    if len > max_len {
        mb_substr_replace(s, suffix, max_len / 2, len - max_len)
    } else {
        s.to_string()
    }
}

/// `arr_qmarks()`: stringify a scalar JSON value the way PHP string coercion
/// would (`"a"` → `a`, `3.5` → `3.5`, `null` → `""`, `true` → `1`).
fn value_to_php_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// PHP `arr_qmarks()`: `["a", "b"]` → `"'a','b'"` (single-quoted, comma-joined).
pub fn arr_qmarks(arr: &[Value]) -> String {
    let mut out = String::new();
    for (i, v) in arr.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('\'');
        out.push_str(&value_to_php_string(v));
        out.push('\'');
    }
    out
}

fn base36_digit(n: u32) -> char {
    match n {
        0..=9 => (b'0' + n as u8) as char,
        _ => (b'a' + (n - 10) as u8) as char,
    }
}

/// PHP `uniqid_short()`: `uniqid()` (hex microtime + 4-hex serial counter) +
/// one random base-36 char. Lowercase alphanumeric, unique in practice.
pub fn uniqid_short() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let micros = now.as_secs().saturating_mul(1_000_000) + now.subsec_micros() as u64;
    let hex = format!("{:x}", micros & 0xFFFF_FFFF_FFFF); // low 48 bits, as `dechex`
    static SERIAL: AtomicU32 = AtomicU32::new(1);
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed) % 10_000;
    let mut rng = rand::thread_rng();
    let b36 = base36_digit(rng.gen_range(0..36));
    format!("{hex}{serial:04x}{b36}")
}

/// PHP `get_random_bytes($length)`: `$length` cryptographically random bytes
/// (PHP's binary string → `Vec<u8>` here).
pub fn get_random_bytes(length: usize) -> Vec<u8> {
    let mut buf = vec![0u8; length];
    rand::thread_rng().fill(&mut buf[..]);
    buf
}

/// A PHP `vsprintf()`: consume the pre-stringified args in order for the
/// value specifiers (`%s`/`%d`/…); `%` escapes and non-value specifiers are
/// kept literally. Unmatched trailing args are ignored (PHP does the same).
fn vsprintf(format: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(format.len());
    let mut ai = 0usize;
    let mut rest = format;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if after.is_empty() {
            out.push('%');
            break;
        }
        if let Some(stripped) = after.strip_prefix('%') {
            out.push('%');
            rest = stripped;
            continue;
        }
        // skip flags / width / precision up to the conversion character
        let bytes = after.as_bytes();
        let mut j = 0;
        while j < bytes.len()
            && matches!(bytes[j], b'-' | b'+' | b'#' | b'0' | b' ' | b'!' | b'.' | b'0'..=b'9')
        {
            j += 1;
        }
        match bytes.get(j).copied() {
            Some(b) if b"sdfeguXo".contains(&b) && ai < args.len() => {
                out.push_str(&args[ai]);
                ai += 1;
                rest = &after[j + 1..];
            }
            Some(_) => {
                // `%` + flags + non-value conversion (or no args left): keep as-is
                out.push('%');
                out.push_str(&after[..j + 1]);
                rest = &after[j + 1..];
            }
            None => {
                // `%` + flags but no conversion char: emit the remainder verbatim
                out.push_str(after);
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// PHP `T_sprintf()`: `vsprintf(__(fmt), args)` — `__()` is the identity with
/// no catalog loaded. Args are pre-stringified (PHP coerces them to strings
/// for `%s`/`%d` alike in the test surface).
pub fn t_sprintf(format: &str, args: &[&str]) -> String {
    vsprintf(format, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

/// PHP `T_nsprintf($singular, $plural, $number, ...args)`: picks the plural
/// form when `$number != 1` (gettext short-circuit, no catalog loaded) and
/// vsprintfs it.
pub fn t_nsprintf(singular: &str, plural: &str, number: i64, args: &[&str]) -> String {
    let chosen = if number != 1 { plural } else { singular };
    vsprintf(chosen, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

/// PHP `gzdecode()`: inflate a gzip-compressed buffer.
pub fn gzdecode(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_string_basics() {
        // test_clean_string_simple
        assert_eq!(clean_str("hello"), "hello");
        // test_clean_string_trimmed
        assert_eq!(clean_str("  hello  "), "hello");
        // test_clean_string_strips_tags
        assert_eq!(clean_str("<b>bold</b> text"), "bold text");
        // test_clean_string_strips_nested_tags
        assert_eq!(clean_str("<div><p>hello</p></div>"), "hello");
        // test_clean_string_strips_attributes
        assert_eq!(
            clean_str(r#"<a href="javascript:alert(1)" onclick="evil()">click me</a>"#),
            "click me"
        );
        // test_clean_string_strips_script
        assert_eq!(clean_str("<script>alert('xss')</script>"), "alert('xss')");
        // test_clean_string_strips_img_onerror
        assert_eq!(clean_str("<img src=x onerror=alert(1)>"), "");
    }

    // NOTE: `approx_constant` allowed — `3.14` is the PHP suite's exact input
    // (1:1 port; the float is a passthrough value, not a constant use).
    #[test]
    #[allow(clippy::approx_constant)]
    fn clean_array_and_passthrough() {
        // test_clean_string_array
        let list: Value = serde_json::json!(
            ["<b>bold</b>", "  plain  ", "<script>bad</script>"]
        );
        assert_eq!(clean(&list), serde_json::json!(["bold", "plain", "bad"]));
        // test_clean_string_array_preserves_keys
        let map: Value = serde_json::json!({"a": "<b>bold</b>", "b": " plain "});
        assert_eq!(clean(&map), serde_json::json!({"a": "bold", "b": "plain"}));
        // test_clean_non_string_passthrough
        assert_eq!(clean(&serde_json::json!(42)), serde_json::json!(42));
        assert_eq!(clean(&serde_json::json!(3.14)), serde_json::json!(3.14));
        assert_eq!(clean(&serde_json::json!(true)), serde_json::json!(true));
        assert_eq!(clean(&serde_json::json!(null)), serde_json::json!(null));
        let mixed: Value = serde_json::json!(["<b>clean</b>", 5, "<i>italic</i>"]);
        assert_eq!(clean(&mixed), serde_json::json!(["clean", 5, "italic"]));
    }

    #[test]
    fn php_trim_differs_from_rust_trim_on_unicode() {
        // PHP's default trim set does not include \u{A0} (non-breaking space).
        assert_eq!(php_trim("\u{A0}x\u{A0}"), "\u{A0}x\u{A0}");
        assert_eq!(php_trim(" \t\n\r\0\u{0B}x"), "x");
    }

    #[test]
    fn mb_substr_replace_matches_php() {
        // test_mb_substr_replace_basic
        assert_eq!(mb_substr_replace("hello world", "XX", 3, 2), "helXX world");
        // test_mb_substr_replace_at_start
        assert_eq!(mb_substr_replace("hello world", "XX", 0, 3), "XXlo world");
        // test_mb_substr_replace_at_end
        assert_eq!(mb_substr_replace("hello world", "XX", 6, 5), "hello XX");
        // test_mb_substr_replace_replaces_all
        assert_eq!(mb_substr_replace("hello world", "", 6, 5), "hello ");
        // test_mb_substr_replace_multibyte
        assert_eq!(mb_substr_replace("café world", "XX", 0, 4), "XX world");
        // test_mb_substr_replace_multibyte_middle
        assert_eq!(mb_substr_replace("café world", "!", 4, 6), "café!");
    }

    #[test]
    fn truncate_middle_matches_php() {
        let d = "&hellip;";
        // test_truncate_middle_no_truncation
        assert_eq!(truncate_middle("hello", 10, d), "hello");
        // test_truncate_middle_exact_length
        assert_eq!(truncate_middle("hello", 5, d), "hello");
        // test_truncate_middle_shortens
        let r = truncate_middle("hello world", 8, d);
        assert!(r.contains(d), "got: {r}");
        assert!(r.contains("hell"), "got: {r}");
        assert!(r.contains("rld"), "got: {r}");
        // test_truncate_middle_with_custom_suffix
        let r = truncate_middle("hello world", 8, "...");
        assert!(r.contains("..."), "got: {r}");
        // test_truncate_middle_empty_string
        assert_eq!(truncate_middle("", 10, d), "");
        // test_truncate_middle_max_len_smaller_than_suffix
        let r = truncate_middle("hello", 2, d);
        assert!(r.contains(d), "got: {r}");
        // test_truncate_middle_multibyte_unicode
        let r = truncate_middle("café world", 8, d);
        assert!(r.contains("café"), "got: {r}");
        assert!(r.contains("rld"), "got: {r}");
    }

    #[test]
    fn arr_qmarks_matches_php() {
        // test_arr_qmarks_single_element
        assert_eq!(arr_qmarks(&[json!("a")]), "'a'");
        // test_arr_qmarks_two_elements
        assert_eq!(arr_qmarks(&[json!("a"), json!("b")]), "'a','b'");
        // test_arr_qmarks_three_elements
        assert_eq!(arr_qmarks(&[json!("a"), json!("b"), json!("c")]), "'a','b','c'");
        // test_arr_qmarks_five_elements
        assert_eq!(
            arr_qmarks(&[json!("a"), json!("b"), json!("c"), json!("d"), json!("e")]),
            "'a','b','c','d','e'"
        );
        // test_arr_qmarks_string_values
        assert_eq!(arr_qmarks(&[json!("1"), json!("2"), json!("3")]), "'1','2','3'");
        // test_arr_qmarks_mixed_values — PHP stringifies int/float elements
        assert_eq!(arr_qmarks(&[json!(1), json!("two"), json!(3.5)]), "'1','two','3.5'");
    }

    #[test]
    fn uniqid_short_matches_php() {
        // test_uniqid_short_returns_string / _non_empty
        let id = uniqid_short();
        assert!(!id.is_empty());
        // test_uniqid_short_alphanumeric
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "got: {id}");
        // test_uniqid_short_uniqueness
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(uniqid_short()), "collision in 10k uniqid_short");
        }
    }

    #[test]
    fn get_random_bytes_matches_php() {
        // test_get_random_bytes_returns_string (length contract)
        for n in [8usize, 16, 256] {
            assert_eq!(get_random_bytes(n).len(), n);
        }
        // test_get_random_bytes_variety
        let a = get_random_bytes(16);
        let b = get_random_bytes(16);
        assert_ne!(a, b, "two 16-byte draws must differ");
        // test_get_random_bytes_large
        assert_eq!(get_random_bytes(1024).len(), 1024);
    }

    #[test]
    fn t_sprintf_matches_php() {
        // test_t_sprintf_format
        assert_eq!(t_sprintf("Hello, %s!", &["User"]), "Hello, User!");
        // test_t_sprintf_multiple_args
        assert_eq!(
            t_sprintf("%d items for %s", &["5", "Bob"]),
            "5 items for Bob"
        );
    }

    #[test]
    fn t_nsprintf_matches_php() {
        // test_t_nsprintf_no_placeholders — the singular/plural selection is
        // the observable behavior (gettext short-circuits: n == 1 → singular).
        assert_eq!(t_nsprintf("One item", "Multiple items", 1, &[]), "One item");
        assert_eq!(t_nsprintf("One item", "Multiple items", 5, &[]), "Multiple items");
        // with placeholders, both forms are vsprintfed
        assert_eq!(
            t_nsprintf("%d item", "%d items", 3, &["3"]),
            "3 items"
        );
    }

    #[test]
    fn gzdecode_matches_php() {
        // test_gzdecode_basic
        let original = "The quick brown fox jumps over the lazy dog. This is a test of gzdecode.";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        encoder.write_all(original.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        let decoded = gzdecode(&compressed).unwrap();
        assert_eq!(std::str::from_utf8(&decoded).unwrap(), original);
    }

    #[test]
    fn strip_tags_basics() {
        assert_eq!(strip_tags("<p>hello</p>"), "hello");
        assert_eq!(strip_tags("a <b>bold</b> &amp; c"), "a bold &amp; c");
        assert_eq!(strip_tags("<!-- comment -->text"), "text");
        assert_eq!(strip_tags("a < 5"), "a < 5");
        assert_eq!(strip_tags("<div>multi\n  line</div>"), "multi\n  line");
    }
}
