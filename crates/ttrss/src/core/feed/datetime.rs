//! Feed date parsing — a simplified port of PHP `strtotime()` for the date
//! formats feeds actually use: RFC 2822 (RSS `pubDate`) and RFC 3339 / ISO
//! 8601 (Atom `updated`/`published`, `dc:date`).

use chrono::DateTime;

/// Parse a feed date string into a Unix timestamp (UTC seconds).
///
/// Simplified `strtotime()`: tries RFC 3339 then RFC 2822. Returns `None` on
/// failure, matching `strtotime`'s `false` for unparseable input.
///
/// NOTE: `strtotime()` is far more permissive than this (it accepts dozens of
/// other formats). The two formats above cover the RSS/Atom date fields; extend
/// here if the functional tests (M6a.5) surface more.
pub fn parse_date(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(dt.timestamp());
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(t) {
        return Some(dt.timestamp());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_date;

    #[test]
    fn parses_rfc2822_pubdate() {
        assert_eq!(
            parse_date("Mon, 01 Jan 2024 12:00:00 GMT"),
            Some(1704110400)
        );
    }

    #[test]
    fn parses_rfc3339_atom_date() {
        assert_eq!(parse_date("2024-01-01T12:00:00Z"), Some(1704110400));
        assert_eq!(parse_date("2024-01-01T10:00:00Z"), Some(1704103200));
    }

    #[test]
    fn fails_on_empty_or_garbage() {
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("   "), None);
        assert_eq!(parse_date("not a date"), None);
    }
}
