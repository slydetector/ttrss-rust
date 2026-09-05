//! Port of `classes/TimeHelper.php` — the subset the M2 read ops use:
//! `make_local_datetime` + `smart_date_time`.
//!
//! tt-rss stores datetimes in UTC; the display value is rendered in the user's
//! timezone: `USER_TIMEZONE == 'Automatic'` → the browser offset
//! (`-$_SESSION['clientTzOffset']`), else the IANA zone's offset at that
//! instant. The "smart date" renderer collapses the rendering to a bare time
//! (same day), a short date (same year), or a long date (otherwise).
//!
//! The M2 callers never pass `no_smart_dt`, so only the smart-date path is
//! ported here.

use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};

/// A `timestamp without time zone` value (stored as UTC) to its PG text
/// representation, e.g. `2026-09-04 12:36:12` or `…12.975486` (fraction only
/// when non-zero, trailing zeros stripped) — the string the PHP `PDO` row
/// carries.
pub fn naive_to_pg_text(dt: &NaiveDateTime) -> String {
    let mut s = dt.format("%Y-%m-%d %H:%M:%S").to_string();
    let f = dt.and_utc().timestamp_subsec_micros();
    if f != 0 {
        let mut frac = format!("{:06}", f);
        while frac.ends_with('0') {
            frac.pop();
        }
        s.push('.');
        s.push_str(&frac);
    }
    s
}

/// The per-request timezone context (the PHP globals/prefs
/// `make_local_datetime` reads).
pub struct TzCtx {
    /// `Prefs::get(Prefs::USER_TIMEZONE, uid)` — "Automatic" or an IANA zone.
    pub user_tz: String,
    /// `$_SESSION['clientTzOffset'] ?? 0` (seconds; applied sign-flipped).
    pub client_tz_offset: i64,
    /// `Prefs::get(Prefs::SHORT_DATE_FORMAT, uid, profile)`.
    pub short_date_format: String,
    /// `Prefs::get(Prefs::LONG_DATE_FORMAT, uid, profile)`.
    pub long_date_format: String,
}

impl TzCtx {
    /// The user's UTC offset (seconds) at the given instant, mirroring the PHP
    /// `DateTimeZone` logic (`Automatic` → browser offset; unknown zone → UTC,
    /// as PHP's `catch` falls back to).
    pub fn offset_at(&self, unix_ts: i64) -> i64 {
        if self.user_tz == "Automatic" {
            return -self.client_tz_offset;
        }
        use chrono::{Offset as _, TimeZone as _};
        use std::str::FromStr;
        if let Ok(tz) = chrono_tz::Tz::from_str(&self.user_tz) {
            if let Some(utc) = DateTime::from_timestamp(unix_ts, 0) {
                let off = tz.offset_from_utc_datetime(&utc.naive_utc());
                return off.fix().local_minus_utc() as i64;
            }
        }
        0 // unknown zone → UTC
    }
}

/// `TimeHelper::make_local_datetime($timestamp, …, $eta_min)` — smart-date path.
pub fn make_local_datetime(timestamp: Option<&str>, eta_min: bool, ctx: &TzCtx) -> String {
    // `if (!$timestamp) $timestamp = '1970-01-01 0:00';`
    let ts = match timestamp {
        Some(s) if !s.is_empty() => s,
        _ => "1970-01-01 0:00",
    };

    // `substr($timestamp, 0, 19)` — "Y-m-d H:i:s" (stored in UTC).
    let Some(ts19) = ts.get(..19.min(ts.len())) else {
        return "Never".into();
    };
    let Ok(dt) = NaiveDateTime::parse_from_str(ts19, "%Y-%m-%d %H:%M:%S") else {
        return "Never".into();
    };

    let utc_ts = dt.and_utc().timestamp();
    let tz_offset = ctx.offset_at(utc_ts);
    let user_ts = utc_ts + tz_offset;

    smart_date_time(user_ts, tz_offset, eta_min, ctx)
}

/// `TimeHelper::smart_date_time($timestamp, $tz_offset, $owner_uid, $eta_min)`.
fn smart_date_time(user_ts: i64, tz_offset: i64, eta_min: bool, ctx: &TzCtx) -> String {
    // `if ($timestamp - $tz_offset === 0) return __('Never');` (the epoch)
    if user_ts - tz_offset == 0 {
        return "Never".into();
    }

    let now_user = Utc::now().timestamp() + tz_offset;

    if eta_min && now_user - user_ts < 3600 {
        // `T_sprintf("%d min", date("i", $diff))` — whole minutes, no padding.
        return format!("{} min", (now_user - user_ts) / 60);
    }

    if same_day(user_ts, now_user) {
        if !ctx.short_date_format.to_lowercase().contains('a') {
            date_fmt("G:i", user_ts) // G:i (24h, no leading zero)
        } else {
            date_fmt("g:i a", user_ts) // g:i a
        }
    } else if same_year(user_ts, now_user) {
        date_fmt(&ctx.short_date_format, user_ts)
    } else {
        date_fmt(&ctx.long_date_format, user_ts)
    }
}

fn same_day(a: i64, b: i64) -> bool {
    day_triple(a) == day_triple(b)
}

fn same_year(a: i64, b: i64) -> bool {
    day_triple(a).0 == day_triple(b).0
}

/// (year, month, day) of a unix timestamp (already in the user's local time).
fn day_triple(ts: i64) -> (i32, u32, u32) {
    let dt = DateTime::from_timestamp(ts, 0).expect("valid ts");
    (dt.year(), dt.month(), dt.day())
}

const MONTHS_SHORT: &[&str; 12] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: &[&str; 12] = &[
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const DAYS_SHORT: &[&str; 7] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAYS_LONG: &[&str; 7] = &[
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

/// A `date($format, $ts)` subset — the characters the tt-rss date-format prefs
/// use (D d j l M n F Y g G h H i a A). Unknown characters pass through
/// verbatim.
pub fn date_fmt(format: &str, ts: i64) -> String {
    let dt = DateTime::from_timestamp(ts, 0).expect("valid ts");
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'D' => out.push_str(DAYS_SHORT[dt.weekday().num_days_from_sunday() as usize]),
            'd' => out.push_str(&format!("{:02}", dt.day())),
            'j' => out.push_str(&dt.day().to_string()),
            'l' => out.push_str(DAYS_LONG[dt.weekday().num_days_from_sunday() as usize]),
            'M' => out.push_str(MONTHS_SHORT[(dt.month() - 1) as usize]),
            'n' => out.push_str(&format!("{:02}", dt.month())),
            'F' => out.push_str(MONTHS_LONG[(dt.month() - 1) as usize]),
            'Y' => out.push_str(&dt.year().to_string()),
            'g' => out.push_str(&g12(&dt).to_string()),
            'G' => out.push_str(&dt.hour().to_string()),
            'h' => out.push_str(&format!("{:02}", g12(&dt))),
            'H' => out.push_str(&format!("{:02}", dt.hour())),
            'i' => out.push_str(&format!("{:02}", dt.minute())),
            'a' => out.push_str(if dt.hour() < 12 { "am" } else { "pm" }),
            'A' => out.push_str(if dt.hour() < 12 { "AM" } else { "PM" }),
            '\\' => {
                if let Some(escaped) = chars.peek() {
                    out.push(*escaped);
                    chars.next();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// PHP `date('g')`: 12-hour clock, no leading zero, 12 for midnight.
fn g12(dt: &DateTime<Utc>) -> u32 {
    match dt.hour() % 12 {
        0 => 12,
        h => h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TzCtx {
        TzCtx {
            user_tz: "Automatic".into(),
            client_tz_offset: 0,
            short_date_format: "j M, G:i".into(),
            long_date_format: "D, M d Y - G:i".into(),
        }
    }

    #[test]
    fn epoch_is_never() {
        // 1970-01-01 00:00 → user_ts - offset == 0 → "Never"
        assert_eq!(
            make_local_datetime(Some("1970-01-01 0:00:00"), false, &ctx()),
            "Never"
        );
        assert_eq!(make_local_datetime(None, false, &ctx()), "Never");
    }

    #[test]
    fn old_date_same_year_uses_short_format() {
        // A date from earlier this year (not today) → "j M, G:i".
        let ts = Utc::now().timestamp() - 14 * 86400;
        let s = make_local_datetime(
            Some(
                &DateTime::from_timestamp(ts, 0)
                    .unwrap()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),
            false,
            &ctx(),
        );
        assert!(
            // `G` is 24-hour with no leading zero, so the hour may be 1–2 digits.
            regex::Regex::new(r"^\d{1,2} (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec), \d{1,2}:\d{2}$")
                .unwrap()
                .is_match(&s),
            "got: {s}"
        );
    }

    #[test]
    fn date_fmt_chars() {
        // 2026-08-20 13:00:00 UTC = Thursday
        let ts = NaiveDateTime::parse_from_str("2026-08-20 13:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc()
            .timestamp();
        assert_eq!(date_fmt("H:i", ts), "13:00");
        assert_eq!(date_fmt("G:i", ts), "13:00");
        let ts3 =
            NaiveDateTime::parse_from_str("2026-08-20 03:05:00", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc()
                .timestamp();
        assert_eq!(date_fmt("G:i", ts3), "3:05");
        assert_eq!(date_fmt("g:i a", ts3), "3:05 am");
        assert_eq!(date_fmt("j M, G:i", ts), "20 Aug, 13:00");
        assert_eq!(date_fmt("D, M d Y - G:i", ts), "Thu, Aug 20 2026 - 13:00");
    }

    #[test]
    fn naive_to_pg_text_strips_trailing_zeros() {
        let dt = NaiveDateTime::parse_from_str("2026-09-04 12:36:12", "%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(naive_to_pg_text(&dt), "2026-09-04 12:36:12");
        let dt = dt + chrono::Duration::microseconds(975_486);
        assert_eq!(naive_to_pg_text(&dt), "2026-09-04 12:36:12.975486");
        let dt = NaiveDateTime::parse_from_str("2026-09-04 12:36:12.500000", "%Y-%m-%d %H:%M:%S%.f")
            .unwrap();
        assert_eq!(naive_to_pg_text(&dt), "2026-09-04 12:36:12.5");
    }
}
