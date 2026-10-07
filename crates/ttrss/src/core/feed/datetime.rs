//! Feed date parsing — a permissive port of PHP `strtotime()` (which PHP's
//! `FeedItem_RSS::get_date` / `FeedItem_Atom::get_date` apply to
//! `pubDate`/`dc:date`/`updated`/`published`) via the `dateparser` crate:
//! RFC 2822, RFC 3339, ISO 8601 variants (space-separated, date-only,
//! partial), `dd Mon yyyy`, `Mon dd, yyyy`, `mm/dd/yyyy`, `yyyy/mm/dd`,
//! unix timestamps, and more.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// Parse a feed date string into a Unix timestamp (UTC seconds).
///
/// `None` on failure, matching `strtotime`'s `false` for unparseable input.
///
/// Zoneless dates resolve to **UTC** — the frozen reference hardcodes
/// `date_default_timezone_set('UTC')` (`include/functions.php:13`), and
/// `dateparser`'s `parse()` default (`chrono::Local`) would not.
///
/// Layered: a deterministic pre-pass handles the date-only / partial forms
/// (PHP gives midnight), then `dateparser` covers the rest (RFC 2822/3339,
/// ISO with time, `dd Mon yyyy`, `Mon dd, yyyy`, `mm/dd/yyyy`, unix
/// timestamps, …). `dateparser` alone is not enough: it anchors date-only
/// input to the *current* time of day (nondeterministic; PHP gives midnight)
/// and lacks the hyphenated/compact forms below.
///
/// Documented deviations from `strtotime()` (no feed sends these in a date
/// field; PHP's own handling is inconsistent there — e.g. `20240115` is a
/// YYYYMMDD date, `1234` a today-anchored time, `1704110400` false, while
/// `dateparser` reads all-digit strings as unix timestamps):
/// - a bare epoch string like `"1704110400"` parses here, `false` in PHP;
/// - time-only strings (`"12:30:45"`) anchor to *today* in both;
/// - date-only + named zone (`"2021-02-21 PST"`) — `dateparser` ignores the
///   zone and anchors the time to now; PHP gives midnight PST.
pub fn parse_date(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }

    // Date-only / partial forms → midnight UTC (the PHP `strtotime` value).
    // 4-digit-year layouts first. The 2-digit-year `mm/dd/yy` precedes
    // `mm/dd/yyyy` because chrono's `%Y` greedily accepts 2 digits (so
    // `8/1/71` would match as year 71 AD); `2024/01/15` never reaches it
    // (%y requires exactly 2 trailing digits).
    let date_only = NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(t, "%Y/%m/%d"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%d-%b-%Y"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%b %d %Y"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%m/%d/%y"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%m/%d/%Y"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%m.%d.%Y"));
    if let Ok(d) = date_only {
        return Some(NaiveDateTime::new(d, NaiveTime::MIN).and_utc().timestamp());
    }
    // ctime (`Mon Jan 15 12:00:00 2024`) — zoneless, midnight-UTC day part
    // with an explicit time.
    if let Ok(dt) = NaiveDateTime::parse_from_str(t, "%a %b %d %H:%M:%S %Y") {
        return Some(dt.and_utc().timestamp());
    }
    // `YYYY-MM` partial → the 1st (PHP `strtotime`'s day default).
    if t.len() == 7 && t.as_bytes()[4] == b'-'
        && let (Ok(y), Ok(m)) = (t[..4].parse::<i32>(), t[5..].parse::<u32>())
            && let Some(d) = NaiveDate::from_ymd_opt(y, m, 1)
        {
            return Some(NaiveDateTime::new(d, NaiveTime::MIN).and_utc().timestamp());
        }

    // Everything else: `dateparser` (zoneless → UTC).
    if let Ok(dt) = dateparser::parse_with_timezone(t, &Utc) {
        return Some(dt.timestamp());
    }
    // dateparser's RFC 3339 arm requires a zone; an ISO `T` separator without
    // one is not in its format list, but `strtotime()` accepts it. Retry with
    // a space separator (a documented dateparser format) — only when the `T`
    // sits between digits (an ISO position), so month names are untouched.
    if let Some(i) = t.find('T')
        && t[..i].chars().next_back().is_some_and(|c| c.is_ascii_digit())
        && t[i + 1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        && let Ok(dt) = dateparser::parse_with_timezone(&t.replacen('T', " ", 1), &Utc) {
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

    /// Permissive formats — expected values are a verbatim oracle of PHP
    /// `strtotime()` (frozen host, default tz UTC), B1. The two today-anchored
    /// PHP quirks (time-only, year-only) are excluded: nondeterministic.
    #[test]
    fn php_strtotime_oracle() {
        let cases: &[(&str, Option<i64>)] = &[
            ("Wed, 02 Jun 2021 06:31:39 GMT", Some(1622615499)),
            ("2024-01-01T10:00:00+02:00", Some(1704096000)),
            ("2024-01-01T12:00:00.123Z", Some(1704110400)),
            ("2024-01-01T12:00:00", Some(1704110400)),
            ("2024-01-01T12:00:00.123", Some(1704110400)),
            ("2024-01-15", Some(1705276800)),
            ("2024-01", Some(1704067200)),
            ("2024-01-15 12:00:00", Some(1705320000)),
            ("2024-01-15 12:30:45", Some(1705321845)),
            ("2024/01/15", Some(1705276800)),
            ("2024/01/15 12:30:45", Some(1705321845)),
            ("15-Jan-2024", Some(1705276800)),
            ("Jan 15 2024", Some(1705276800)),
            ("Mon Jan 15 12:00:00 2024", Some(1705320000)),
            ("01/15/2024", Some(1705276800)),
            ("8/1/71", Some(49852800)),
            ("03.31.2014", Some(1396224000)),
            ("15/01/2024", None), // slash = MM/DD; 15 is not a month
            ("2024-13-45", None),
            ("2021-02-21", Some(1613865600)),
        ];
        for (s, want) in cases {
            assert_eq!(parse_date(s), *want, "strtotime oracle: {s:?}");
        }
    }

    /// Documented deviations from `strtotime()` (see the fn doc).
    #[test]
    fn documented_php_deviations() {
        assert_eq!(
            parse_date("1704110400"),
            Some(1704110400),
            "bare epoch: dateparser accepts, PHP returns false"
        );
        // Date-only + named zone: `dateparser` anchors the time to now and
        // ignores the zone (PHP: midnight PST, 1613894400). Non-deterministic
        // → assert presence only.
        assert!(parse_date("2021-02-21 PST").is_some());
    }
}
