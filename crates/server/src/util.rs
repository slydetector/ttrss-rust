//! Ports of PHP built-ins shared across server modules.
//!
//! The PHP reference calls the single global `strip_tags()` from many places
//! (sanitizer's `clean()`, `Feeds` view params, `Prefs::_set`, …); the Rust
//! port lives here once and is reused by every module.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_basics() {
        assert_eq!(strip_tags("<p>hello</p>"), "hello");
        assert_eq!(strip_tags("a <b>bold</b> &amp; c"), "a bold &amp; c");
        assert_eq!(strip_tags("<!-- comment -->text"), "text");
        assert_eq!(strip_tags("a < 5"), "a < 5");
        assert_eq!(strip_tags("<div>multi\n  line</div>"), "multi\n  line");
    }
}
