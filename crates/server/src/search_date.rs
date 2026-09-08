//! PHP `DateTime` date parsing for the `@date` search keyword.
//!
//! `Feeds::_search_to_sql` resolves `@<date>` via `new DateTime($date, $tz)`
//! and keeps only the calendar day (first/last second of that day, UTC).
//! PHP's relative formats support e.g. `2024-11-28`, `2025/11/28`,
//! `16-11-2025`, `17 nov 2025`, `november 17` (current year), `today`,
//! `yesterday`, `last monday`, `2 days ago`, etc.
//!
//! Ported subset (each rule verified against the PHP 8.5 oracle):
//! - `@epoch` (digits only, UTC), `now` / `today` / `yesterday` / `tomorrow`
//! - a weekday name (next occurrence, today included); `last <wd>` /
//!   `next <wd>` (today is that weekday → ±7 days); `last week` / `next week`
//!   (now ∓/± 7 days), `this week` (today)
//! - `±N day|week|month|year[s] [ago|from now]` — PHP's sign semantics:
//!   `-5 days ago` is +5 (double negation); calendar month/year math with
//!   overflow (May 31 − 1 month → May 1)
//! - `Y-m-d`, `Y/m/d` (4-digit year first; optional `[Tt ]+` time — bare
//!   `T`, hour-only, `24:00` (next day), `ss:60` cascades, as in PHP); `Y-m` (day 1)
//!   and bare `Y` (keeps the current day/month); time-only (`13:45` → today)
//! - three numbers `a-b-c` (dash/dot): `b>12` → invalid, else d-m-Y (PHP
//!   oracle: `16-11-2025` → Nov 16, `01-02-2026` → 2026-02-01; `11-28-2026`
//!   / `1-28-2026` / `13-14-2026` are errors)
//! - `M d` / `d M` (current year) and `M d, Y` / `d M Y` (month full or
//!   3-letter, optional day ordinal)
//! - day overflow (`2026-02-29` → 2026-03-01) and day 0 (`2026-02-00` →
//!   2026-01-31); month must be 1–12 in absolute forms
//!
//! Anything else is `None` (PHP throws; the caller emits PHP's
//! `( will generate an error )` fragment + warning word).

use chrono::{Datelike, DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use regex::Regex;
use std::sync::LazyLock;

const MONTHS: [&str; 12] = [
    "january", "february", "march", "april", "may", "june", "july", "august",
    "september", "october", "november", "december",
];
const WEEKDAYS: [&str; 14] = [
    "monday", "mon", "tuesday", "tue", "wednesday", "wed", "thursday", "thu",
    "friday", "fri", "saturday", "sat", "sunday", "sun",
];

fn weekday_idx(name: &str) -> Option<i32> {
    WEEKDAYS
        .iter()
        .position(|w| *w == name)
        .map(|p| (p / 2) as i32)
}

fn month_idx(name: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|m| *m == name || (name.len() >= 3 && m.starts_with(name)))
        .map(|p| (p + 1) as u32)
}

/// A date built from year/month with PHP's day overflow (day 0 → previous
/// month's last day; day > days_in_month → carried forward).
fn ymd_overflow(y: i32, m: u32, d: i32) -> Option<NaiveDate> {
    if m > 12 || y == i32::MIN {
        return None;
    }
    let (y, m) = if m == 0 { (y.checked_sub(1)?, 12) } else { (y, m) };
    let base = NaiveDate::from_ymd_opt(y, m, 1)?;
    Some(base + Duration::days((d - 1) as i64))
}

/// PHP calendar month/year arithmetic with overflow (May 31 − 1 month →
/// May 1).
fn month_add(d: NaiveDate, months: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + d.month() as i64 - 1 + months;
    let y = total.div_euclid(12) as i32;
    let m = (total.rem_euclid(12) + 1) as u32;
    let base = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    base + Duration::days(d.day() as i64 - 1)
}

/// The calendar day (in the user's zone) for a PHP `DateTime` query, or
/// `None` when PHP would throw.
pub fn php_date_day(query: &str, user_tz: &str) -> Option<NaiveDate> {
    let tz: chrono_tz::Tz = user_tz.parse().unwrap_or(chrono_tz::Tz::UTC);
    let s = query.trim();
    // `@epoch` — PHP interprets the epoch in UTC, then renders in the zone.
    if let Some(rest) = s.strip_prefix('@') {
        let secs: i64 = rest.parse().ok()?;
        let dt = chrono::DateTime::from_timestamp(secs, 0)?;
        return Some(dt.with_timezone(&tz).date_naive());
    }
    let today = Utc::now().with_timezone(&tz).date_naive();
    parse_date(s, today)
}

/// The pure parse (relative to a fixed `today`) — the unit-tested core.
pub fn parse_date(query: &str, today: NaiveDate) -> Option<NaiveDate> {
    let s = query.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_lowercase();

    match lower.as_str() {
        "now" | "today" => return Some(today),
        "yesterday" => return Some(today - Duration::days(1)),
        "tomorrow" => return Some(today + Duration::days(1)),
        // PHP "last/next week" = now ∓/± 7 days; "this week" = now.
        "last week" => return Some(today - Duration::days(7)),
        "next week" => return Some(today + Duration::days(7)),
        "this week" => return Some(today),
        _ => {}
    }

    // A bare weekday name: next occurrence, today included.
    if let Some(wd) = weekday_idx(&lower) {
        let today_wd = today.weekday().num_days_from_monday() as i32;
        return Some(today + Duration::days(((wd - today_wd + 7) % 7) as i64));
    }

    // `last <weekday>` / `next <weekday>` (today is that weekday → ±7 days).
    if let Some(d) = parse_last_next_weekday(&lower, today) {
        return Some(d);
    }

    if let Some(d) = parse_relative(&lower, today) {
        return Some(d);
    }

    parse_absolute(&lower, today)
}

fn parse_last_next_weekday(lower: &str, today: NaiveDate) -> Option<NaiveDate> {
    let mut it = lower.split_whitespace();
    let (prefix, wd_name) = (it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let wd = weekday_idx(wd_name)?;
    let today_wd = today.weekday().num_days_from_monday() as i32;
    let diff = (wd - today_wd).rem_euclid(7);
    let days = if diff == 0 { 7 } else if prefix == "last" { 7 - diff } else if prefix == "next" { diff } else { return None };
    let days_i64 = if prefix == "last" { -(days as i64) } else { days as i64 };
    Some(today + Duration::days(days_i64))
}

/// `2 days`, `-5 days`, `2 days ago` (double negation: `-5 days ago` = +5),
/// `3 weeks from now`, `1 month ago`, `2 years`.
fn parse_relative(lower: &str, today: NaiveDate) -> Option<NaiveDate> {
    let (neg, rest) = lower.strip_prefix('-').map(|r| (true, r)).unwrap_or((false, lower));
    let mut it = rest.split_whitespace();
    let first = it.next()?;
    // `last <unit>` / `next <unit>` = implicit N=1 (PHP: `last month` =
    // now − 1 calendar month, not the first of the previous month).
    let (mut n, unit) = if first == "last" || first == "next" {
        (if first == "last" { -1 } else { 1 }, it.next()?)
    } else {
        (first.parse::<i64>().ok()?, it.next()?)
    };
    if neg {
        n = -n;
    }
    let (day_unit, months) = match unit {
        "day" | "days" => (Some(1i64), 0),
        "week" | "weeks" => (Some(7), 0),
        "month" | "months" => (None, 1),
        "year" | "years" => (None, 12),
        _ => return None,
    };
    let tail = it.next();
    let valid_tail = match tail {
        None | Some("ago") => true,
        Some("from") => it.next() == Some("now"),
        _ => false,
    };
    if !valid_tail {
        return None;
    }
    if tail == Some("ago") {
        n = -n;
    }
    if let Some(u) = day_unit {
        return Some(today + Duration::days(n * u));
    }
    Some(month_add(today, n * months as i64))
}

static RE_YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        "^(?:([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})|([0-9]{4})/([0-9]{1,2})/([0-9]{1,2}))",
        "(?:[Tt ]+(?:(?:([0-9]{1,2})(?::([0-9]{1,2})(?::([0-9]{1,2}))))",
            "|(?:([0-9]{1,2})(?::([0-9]{1,2})))|(?:([0-9]{1,2})))?)?$",
    ))
    .unwrap()
});
static RE_YM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{4}-[0-9]{1,2}$").unwrap());
static RE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{4}$").unwrap());
static RE_3NUM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(\d{1,2})-(\d{1,2})-(\d{4})|(\d{1,2})\.(\d{1,2})\.(\d{4}))$").unwrap()
});
static RE_TIME_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        "^(?:(?:([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2}))",
            "|(?:([0-9]{1,2}):([0-9]{1,2})))$",
    ))
    .unwrap()
});
static RE_MD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        "^(?:([a-z]+) (\\d{1,2})(?:st|nd|rd|th)?,? ([0-9]{4})",
        "|([a-z]+) (\\d{1,2})(?:st|nd|rd|th)?",
        "|(\\d{1,2})(?:st|nd|rd|th)? ([a-z]+),? ([0-9]{4})",
        "|(\\d{1,2})(?:st|nd|rd|th)? ([a-z]+))$",
    ))
    .unwrap()
});

/// PHP's time-part validation with cascading overflow. Returns the number
/// of days the time pushes the calendar day forward (1 when the hour resolves
/// to 24/25), or `None` when PHP would reject the time: input `h ≤ 24`,
/// `m ≤ 59`, `s ≤ 60` (60 rolls into the minute; 59:60 rolls the hour).
fn time_day_shift(h: u32, mi: u32, s: u32) -> Option<u32> {
    if h > 24 || mi > 59 || s > 60 {
        return None;
    }
    let mut h = h;
    let mut mi = mi;
    if s == 60 {
        mi += 1;
    }
    if mi == 60 {
        h += 1;
    }
    Some(u32::from(h >= 24))
}

fn parse_absolute(lower: &str, today: NaiveDate) -> Option<NaiveDate> {
    // `Y-m-d[time]` / `Y/m/d[time]` (4-digit year first only).
    if let Some(c) = RE_YMD.captures(lower) {
        let (y, m, d) = if c.get(1).is_some() {
            (c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)
        } else {
            (c[4].parse().ok()?, c[5].parse().ok()?, c[6].parse().ok()?)
        };
        let (h_g, mi_g, s_g) = if c.get(7).is_some() {
            (c.get(7), c.get(8), c.get(9))
        } else if c.get(10).is_some() {
            (c.get(10), c.get(11), None)
        } else {
            (c.get(12), None, None)
        };
        let shift = if let Some(h) = h_g {
            let h: u32 = h.as_str().parse().ok()?;
            let mi = mi_g.and_then(|x| x.as_str().parse::<u32>().ok()).unwrap_or(0);
            let s = s_g.and_then(|x| x.as_str().parse::<u32>().ok()).unwrap_or(0);
            time_day_shift(h, mi, s)?
        } else {
            0
        };
        let base = ymd_overflow(y, m, d)?;
        return Some(base + Duration::days(shift as i64));
    }

    // Time-only (`0:30`, `13:45:30`) → today at that time.
    if RE_TIME_ONLY.is_match(lower) {
        let mut it = lower.split(':');
        let h: u32 = it.next()?.parse().ok()?;
        let mi = it.next().and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
        let s = it.next().and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
        return Some(today + Duration::days(time_day_shift(h, mi, s)? as i64));
    }

    // `Y-m` (day 1) and bare `Y` (keeps the current month/day).
    if RE_YM.is_match(lower) {
        let mut it = lower.splitn(2, '-');
        let y: i32 = it.next()?.parse().ok()?;
        let m: u32 = it.next()?.parse().ok()?;
        return ymd_overflow(y, m, 1);
    }
    if let Some(y) = RE_YEAR.captures(lower) {
        let y: i32 = y[0].parse().ok()?;
        return ymd_overflow(y, today.month(), today.day() as i32);
    }

    // Three numbers, 4-digit year last: a>12 → d-m-Y; b>12 → invalid;
    // else m-d-Y (PHP's dash/dot ambiguity rules).
    if let Some(c) = RE_3NUM.captures(lower) {
        let (a, b, yy): (i32, i32, i32) = if c.get(1).is_some() {
            (c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)
        } else {
            (c[4].parse().ok()?, c[5].parse().ok()?, c[6].parse().ok()?)
        };
        // PHP: `b > 12` is invalid either way; otherwise d-m-Y (even the
        // ambiguous `01-02-2026` → 2026-02-01).
        if b > 12 {
            return None;
        }
        return ymd_overflow(yy, b as u32, a);
    }

    // Month-name forms: `M d Y`, `M d`, `d M Y`, `d M` (current year).
    if let Some(c) = RE_MD.captures(lower) {
        let (m_name, d_s, yy) = if c.get(1).is_some() {
            (&c[1], c[2].parse().ok()?, Some(c[3].parse().ok()?))
        } else if c.get(4).is_some() {
            (&c[4], c[5].parse().ok()?, None)
        } else if c.get(6).is_some() {
            (&c[7], c[6].parse().ok()?, Some(c[8].parse().ok()?))
        } else {
            (&c[10], c[9].parse().ok()?, None)
        };
        let m = month_idx(m_name)?;
        let yy = yy.unwrap_or(today.year());
        return ymd_overflow(yy, m, d_s);
    }

    None
}

/// `[first, last]` UTC wall-clock strings (`Y-m-d H:i:s`) bounding the
/// calendar day in the user's zone — PHP's
/// `date('Y-m-d H:i:s', $dt->setTime(0,0,0)->format('U'))` (UTC default tz).
pub fn day_range_utc(day: NaiveDate, user_tz: &str) -> (String, String) {
    let tz: chrono_tz::Tz = user_tz.parse().unwrap_or(chrono_tz::Tz::UTC);
    // PHP `DateTime` normalization: an ambiguous local time keeps the first
    // (older) offset; a nonexistent local time is shifted past the gap.
    let resolve = |naive: NaiveDateTime| match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Some(dt),
        LocalResult::None => tz.from_local_datetime(&naive).latest(),
    };
    let first = resolve(day.and_time(NaiveTime::MIN)).unwrap();
    let last = resolve(day.and_hms_opt(23, 59, 59).unwrap()).unwrap();
    let fmt = |dt: DateTime<chrono_tz::Tz>| {
        let utc: DateTime<Utc> = dt.with_timezone(&Utc);
        utc.format("%Y-%m-%d %H:%M:%S").to_string()
    };
    (fmt(first), fmt(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-07 is a Monday (the date the oracle references were taken
    /// on; the parser is pure relative to `today`, and Monday exercises the
    /// `last/next <wd>` edges).
    fn t() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 7).unwrap()
    }
    fn p(s: &str) -> NaiveDate {
        parse_date(s, t()).unwrap_or_else(|| panic!("parse {s:?}"))
    }
    fn p_opt(s: &str) -> Option<NaiveDate> {
        parse_date(s, t())
    }
    fn d(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn bare_keywords() {
        assert_eq!(p("now"), d(2026, 9, 7));
        assert_eq!(p("today"), d(2026, 9, 7));
        assert_eq!(p("yesterday"), d(2026, 9, 6));
        assert_eq!(p("tomorrow"), d(2026, 9, 8));
        assert_eq!(p("YESTERDAY"), d(2026, 9, 6));
    }

    #[test]
    fn weeks() {
        assert_eq!(p("last week"), d(2026, 8, 31)); // now − 7 days
        assert_eq!(p("next week"), d(2026, 9, 14));
        assert_eq!(p("this week"), d(2026, 9, 7));
    }

    #[test]
    fn weekday_names() {
        // today (Mon) included; forward-only within the next 7 days.
        assert_eq!(p("monday"), d(2026, 9, 7));
        assert_eq!(p("friday"), d(2026, 9, 11));
        assert_eq!(p("sunday"), d(2026, 9, 13));
        assert_eq!(p("fri"), d(2026, 9, 11));
        assert_eq!(p("FRI"), d(2026, 9, 11));
        // 3-char prefix must not false-match ("mon" ok, "mo" not a weekday)
        assert_eq!(p("mon"), d(2026, 9, 7));
        assert_eq!(p_opt("mo"), None);
    }

    #[test]
    fn last_next_weekday() {
        assert_eq!(p("last monday"), d(2026, 8, 31)); // today is Monday
        assert_eq!(p("last tuesday"), d(2026, 9, 1));
        assert_eq!(p("next monday"), d(2026, 9, 14));
        assert_eq!(p("next friday"), d(2026, 9, 11));
        assert_eq!(p("LAST MONDAY"), d(2026, 8, 31));
    }

    #[test]
    fn relative_units() {
        assert_eq!(p("2 days ago"), d(2026, 9, 5));
        assert_eq!(p("3 weeks ago"), d(2026, 8, 17));
        assert_eq!(p("1 month ago"), d(2026, 8, 7));
        assert_eq!(p("1 year ago"), d(2025, 9, 7));
        assert_eq!(p("18 months ago"), d(2025, 3, 7));
        assert_eq!(p("-5 days"), d(2026, 9, 2));
        // double negation (PHP oracle: `-5 days ago` → +5)
        assert_eq!(p("-5 days ago"), d(2026, 9, 12));
        assert_eq!(p("2 days"), d(2026, 9, 9)); // bare = forward
        assert_eq!(p("3 weeks"), d(2026, 9, 28));
        assert_eq!(p("2 days from now"), d(2026, 9, 9));
        // `last month` = calendar −1 month (not "first of previous month")
        assert_eq!(p("last month"), d(2026, 8, 7));
        assert_eq!(p("next month"), d(2026, 10, 7));
        // calendar month overflow (PHP: Apr 31 → May 1)
        let may31 = d(2026, 5, 31);
        assert_eq!(parse_date("1 month ago", may31), Some(d(2026, 5, 1)));
        // negative month across year
        assert_eq!(parse_date("1 month ago", d(2026, 1, 15)), Some(d(2025, 12, 15)));
        // unknown unit / bad tail → not a relative date
        assert_eq!(p_opt("2 bananas"), None);
        assert_eq!(p_opt("2 days extra"), None);
        assert_eq!(p_opt("last 5 days"), None);
    }

    #[test]
    fn absolute_ymd() {
        assert_eq!(p("2026-05-25"), d(2026, 5, 25));
        assert_eq!(p("2026/05/25"), d(2026, 5, 25));
        assert_eq!(p("2026-5-5"), d(2026, 5, 5));
        assert_eq!(p("2026-05-25T13:45"), d(2026, 5, 25));
        assert_eq!(p("2026-05-25t13:45"), d(2026, 5, 25)); // lowercase t
        assert_eq!(p("2026-05-25  13:45:30"), d(2026, 5, 25)); // double space
        assert_eq!(p("2026-05-25 0:30"), d(2026, 5, 25));
        assert_eq!(p("2026-05-25T"), d(2026, 5, 25)); // bare T
        assert_eq!(p("2026-05-25T13"), d(2026, 5, 25)); // hour only
        assert_eq!(p("2026-05"), d(2026, 5, 1));
        assert_eq!(p("2026-5"), d(2026, 5, 1));
        // bare year keeps the current month/day (PHP oracle: @2026 → today)
        assert_eq!(p("2026"), d(2026, 9, 7));
        // time validity: 24:00 ok, 25:00 not; ss:60 ok, :99 not
        assert_eq!(p("2026-05-25 24:00"), d(2026, 5, 26)); // 24:00 → next day
        assert_eq!(p("2026-05-25T24"), d(2026, 5, 26));
        assert_eq!(p("2026-05-25 13:59:60"), d(2026, 5, 25)); // → 14:00
        assert_eq!(p("2026-05-25T24:59:60"), d(2026, 5, 26)); // → 01:00 next day
        assert_eq!(p_opt("2026-05-25T24:60"), None);
        assert_eq!(p_opt("2026-05-25T13:60:00"), None);
        assert_eq!(p_opt("2026-05-25 25:00"), None);
        assert_eq!(p("2026-05-25 13:45:60"), d(2026, 5, 25));
        assert_eq!(p_opt("5:00:99"), None);
        assert_eq!(p_opt("2026-13-05"), None);
        assert_eq!(p_opt("2026.05.25"), None); // dot year-first unsupported
        // time-only → today (24:00 → tomorrow)
        assert_eq!(p("0:30"), d(2026, 9, 7));
        assert_eq!(p("13:45:30"), d(2026, 9, 7));
        assert_eq!(p("23:59:60"), d(2026, 9, 8));
        assert_eq!(p("24:00"), d(2026, 9, 8));
        assert_eq!(p_opt("61:00"), None);
        assert_eq!(p_opt("61:00"), None);
    }

    #[test]
    fn absolute_overflow() {
        // PHP rolls invalid days forward (Feb 29 2026 → Mar 1) and day 0
        // back to the previous month's last day.
        assert_eq!(p("2026-02-29"), d(2026, 3, 1));
        assert_eq!(p("2026-02-00"), d(2026, 1, 31));
        assert_eq!(p("2026-12-32"), d(2027, 1, 1));
    }

    #[test]
    fn three_numbers() {
        assert_eq!(p("16-11-2025"), d(2025, 11, 16)); // a>12 → d-m-Y
        assert_eq!(p("28-11-2026"), d(2026, 11, 28));
        assert_eq!(p("01-02-2026"), d(2026, 2, 1)); // ambiguous → d-m-Y
        assert_eq!(p("05-05-2026"), d(2026, 5, 5));
        assert_eq!(p("5-5-2026"), d(2026, 5, 5));
        // PHP: b>12 with a≤12 is an error (not m-d-Y)
        assert_eq!(p_opt("11-28-2026"), None);
        assert_eq!(p_opt("1-28-2026"), None);
        assert_eq!(p_opt("13-14-2026"), None); // both > 12
        // day/month 0 roll back (PHP oracle)
        assert_eq!(p("0-0-2026"), d(2025, 11, 30));
        assert_eq!(p("0-1-2026"), d(2025, 12, 31));
        // dot form: same rules (d-m-Y)
        assert_eq!(p("1.2.2026"), d(2026, 2, 1));
        assert_eq!(p("28.1.2026"), d(2026, 1, 28));
        assert_eq!(p("5.5.2026"), d(2026, 5, 5));
    }
    #[test]
    fn month_names() {
        assert_eq!(p("17 nov 2025"), d(2025, 11, 17));
        assert_eq!(p("17th nov 2025"), d(2025, 11, 17));
        assert_eq!(p("1st jan 2026"), d(2026, 1, 1));
        assert_eq!(p("november 17"), d(2026, 11, 17)); // current year
        assert_eq!(p("nov 17"), d(2026, 11, 17));
        assert_eq!(p("jan 31st"), d(2026, 1, 31));
        assert_eq!(p("Jan 1, 2026"), d(2026, 1, 1));
        assert_eq!(p("jan 1"), d(2026, 1, 1));
        assert_eq!(p("Nov 17 2026"), d(2026, 11, 17));
        // overflow: Nov 31 → Dec 1
        assert_eq!(p("november 31"), d(2026, 12, 1));
        // not a month
        assert_eq!(p_opt("foo 1"), None);
    }

    #[test]
    fn invalid_inputs() {
        for s in [
            "", "  ", "bogus", "last", "next", "2", "2 days extra",
            "tomorrow at 3pm", "2026-05-31 minus 1 month", "11-28",
            "2026-05-25 13:", "mon day",
        ] {
            assert_eq!(parse_date(s, t()), None, "expected invalid: {s:?}");
        }
    }

    #[test]
    fn day_range() {
        // Verified against the PHP oracle (`date('Y-m-d H:i:s', ts)` in the
        // server default UTC zone).
        assert_eq!(
            day_range_utc(d(2026, 5, 25), "UTC"),
            ("2026-05-25 00:00:00".into(), "2026-05-25 23:59:59".into())
        );
        // Fixed +02:00 offset (Etc signs are inverted).
        assert_eq!(
            day_range_utc(d(2026, 5, 25), "Etc/GMT-2"),
            ("2026-05-24 22:00:00".into(), "2026-05-25 21:59:59".into())
        );
        // Iran's midnight spring-forward: 2026-03-27 00:00 Tehran is
        // nonexistent → PHP shifts past the gap (01:00 at +04:30).
        assert_eq!(
            day_range_utc(d(2026, 3, 27), "Asia/Tehran"),
            ("2026-03-26 20:30:00".into(), "2026-03-27 20:29:59".into())
        );
        // A plain Tehran day (+03:30).
        assert_eq!(
            day_range_utc(d(2026, 3, 19), "Asia/Tehran"),
            ("2026-03-18 20:30:00".into(), "2026-03-19 20:29:59".into())
        );
        // US fall-back day: 25h in UTC wall clock.
        assert_eq!(
            day_range_utc(d(2026, 11, 1), "America/New_York"),
            ("2026-11-01 04:00:00".into(), "2026-11-02 04:59:59".into())
        );
    }
}
