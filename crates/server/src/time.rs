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
use crate::prefs;

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

impl TzCtx {
    /// Build the context for the current session (loads the owner's date/time
    /// prefs once). Handlers that render dates via `make_local_datetime` use
    /// this instead of inlining the `prefs::load_rows` + `resolve` sequence.
    pub async fn for_session(state: &crate::AppState, data: &crate::core::session::Session) -> Self {
        let uid = data.uid().unwrap_or(0);
        let profile = data.profile().map(|p| p as i32);
        let rows = crate::prefs::load_rows(&state.db, uid, profile).await;
        Self {
            user_tz: crate::prefs::resolve(prefs::USER_TIMEZONE, &rows, profile)
                .as_str()
                .to_string(),
            client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
            short_date_format: crate::prefs::resolve(prefs::SHORT_DATE_FORMAT, &rows, profile)
                .as_str()
                .to_string(),
            long_date_format: crate::prefs::resolve(prefs::LONG_DATE_FORMAT, &rows, profile)
                .as_str()
                .to_string(),
        }
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
/// use (D d j l M m n F Y g G h H i s a A). Unknown characters pass through
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
            'm' => out.push_str(&format!("{:02}", dt.month())),
            'n' => out.push_str(&dt.month().to_string()),
            'F' => out.push_str(MONTHS_LONG[(dt.month() - 1) as usize]),
            'Y' => out.push_str(&dt.year().to_string()),
            'g' => out.push_str(&g12(&dt).to_string()),
            'G' => out.push_str(&dt.hour().to_string()),
            'h' => out.push_str(&format!("{:02}", g12(&dt))),
            'H' => out.push_str(&format!("{:02}", dt.hour())),
            'i' => out.push_str(&format!("{:02}", dt.minute())),
            's' => out.push_str(&format!("{:02}", dt.second())),
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

    // ── Ported 1:1 from tt-rss/tests_integration/TimeHelperTest.php ──────────
    // The PHP suite's setUp pins USER_TIMEZONE='UTC', SHORT_DATE_FORMAT='M d,
    // G:i', LONG_DATE_FORMAT='D, M d Y - G:i', and an unset clientTzOffset.
    // We mirror that context so the expectations match the PHP suite exactly.
    // (Cases that exercise features the Rust port omits — `$long`/`$no_smart_dt`,
    // explicit `$owner_uid`/profile, and `convert_timestamp` — are listed in the
    // plan's test-parity table and are N/A until those are ported.)
    fn php_ctx() -> TzCtx {
        TzCtx {
            user_tz: "UTC".into(),
            client_tz_offset: 0,
            short_date_format: "M d, G:i".into(),
            long_date_format: "D, M d Y - G:i".into(),
        }
    }

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn smart_never() {
        // test_smart_date_time_never: epoch → "Never".
        assert_eq!(smart_date_time(0, 0, false, &php_ctx()), "Never");
    }

    #[test]
    fn smart_never_with_tz_offset() {
        // test_smart_date_time_never_with_tz_offset: timestamp − tz_offset == 0.
        assert_eq!(smart_date_time(3600, 3600, false, &php_ctx()), "Never");
    }

    #[test]
    fn smart_same_day_24h() {
        // test_smart_date_time_same_day_24h: same day, no 'a' → "G:i".
        let now = Utc::now().timestamp();
        let r = smart_date_time(now, 0, false, &php_ctx());
        assert!(
            regex::Regex::new(r"^\d{1,2}:\d{2}$").unwrap().is_match(&r),
            "got: {r}"
        );
        assert_eq!(r, date_fmt("G:i", now));
    }

    #[test]
    fn smart_same_day_12h() {
        // test_smart_date_time_same_day_12h: same day, 'a' present → "g:i a".
        let now = Utc::now().timestamp();
        let mut ctx = php_ctx();
        ctx.short_date_format = "M d, g:i a".into();
        let r = smart_date_time(now, 0, false, &ctx);
        assert!(
            regex::Regex::new(r"^\d{1,2}:\d{2} [ap]m$").unwrap().is_match(&r),
            "got: {r}"
        );
        assert_eq!(r, date_fmt("g:i a", now));
    }

    #[test]
    fn smart_same_year() {
        // test_smart_date_time_same_year: earlier this year → "Jan 15, 10:00".
        let this_year = Utc::now().year();
        assert_eq!(smart_date_time(ts(this_year, 1, 15, 10, 0), 0, false, &php_ctx()), "Jan 15, 10:00");
    }

    #[test]
    fn smart_different_year() {
        // test_smart_date_time_different_year: 2020 → LONG format.
        assert_eq!(
            smart_date_time(ts(2020, 6, 15, 14, 30), 0, false, &php_ctx()),
            "Mon, Jun 15 2020 - 14:30"
        );
    }

    #[test]
    fn smart_eta_min_within_hour() {
        // test_smart_date_time_eta_min_within_hour: 30 min ago → "30 min".
        let past = Utc::now().timestamp() - 30 * 60;
        assert_eq!(smart_date_time(past, 0, true, &php_ctx()), "30 min");
    }

    #[test]
    fn smart_eta_min_less_than_5_min() {
        // test_smart_date_time_eta_min_less_than_5_minutes: 4 min ago → "4 min".
        let past = Utc::now().timestamp() - 4 * 60;
        assert_eq!(smart_date_time(past, 0, true, &php_ctx()), "4 min");
    }

    #[test]
    fn smart_eta_min_over_hour() {
        // test_smart_date_time_eta_min_over_hour: 2h ago → falls through, no "min".
        let past = Utc::now().timestamp() - 2 * 3600;
        let r = smart_date_time(past, 0, true, &php_ctx());
        assert!(!r.contains("min"), "got: {r}");
    }

    #[test]
    fn smart_zero_tz_offset() {
        // test_smart_date_time_zero_tz_offset: same day with zero offset.
        let now = Utc::now().timestamp();
        let r = smart_date_time(now, 0, false, &php_ctx());
        assert!(regex::Regex::new(r"^\d{1,2}:\d{2}$").unwrap().is_match(&r), "got: {r}");
    }

    #[test]
    fn smart_custom_short_format() {
        // test_smart_date_time_custom_short_format: SHORT 'Y-m-d H:i' (no 'a')
        // → same day still collapses to bare "G:i".
        let now = Utc::now().timestamp();
        let mut ctx = php_ctx();
        ctx.short_date_format = "Y-m-d H:i".into();
        assert_eq!(smart_date_time(now, 0, false, &ctx), date_fmt("G:i", now));
    }

    #[test]
    fn smart_custom_long_format() {
        // test_smart_date_time_custom_long_format: LONG 'Y-m-d H:i:s' → full.
        let mut ctx = php_ctx();
        ctx.long_date_format = "Y-m-d H:i:s".into();
        assert_eq!(
            smart_date_time(ts(2020, 6, 15, 14, 30), 0, false, &ctx),
            "2020-06-15 14:30:00"
        );
    }

    #[test]
    fn make_local_null_timestamp() {
        // test_make_local_datetime_null_timestamp: null → non-empty (epoch).
        assert_eq!(make_local_datetime(None, false, &php_ctx()), "Never");
    }

    #[test]
    fn make_local_empty_string() {
        // test_make_local_datetime_empty_string_timestamp: empty → non-empty.
        assert_eq!(make_local_datetime(Some(""), false, &php_ctx()), "Never");
    }

    #[test]
    fn make_local_short_format_default() {
        // test_make_local_datetime_short_format_default: a *now* timestamp with
        // the default context collapses to a bare time.
        let now = Utc::now().timestamp();
        let s = date_fmt("Y-m-d H:i:s", now);
        let r = make_local_datetime(Some(&s), false, &php_ctx());
        assert!(regex::Regex::new(r"^\d{1,2}:\d{2}$").unwrap().is_match(&r), "got: {r}");
    }

    #[test]
    fn make_local_truncates_to_19_chars() {
        // test_make_local_datetime_truncates_to_19_chars: microsecond fraction is
        // trimmed to the 19-char "Y-m-d H:i:s" before parsing (no crash).
        let r = make_local_datetime(Some("2026-05-27 14:30:45.123456"), false, &php_ctx());
        assert!(!r.is_empty());
    }

    #[test]
    fn make_local_with_fixed_timezone() {
        // test_make_local_datetime_with_fixed_timezone: a real IANA zone renders.
        let mut ctx = php_ctx();
        ctx.user_tz = "Europe/London".into();
        let r = make_local_datetime(Some(&date_fmt("Y-m-d H:i:s", Utc::now().timestamp())), false, &ctx);
        assert!(!r.is_empty());
    }

    #[test]
    fn make_local_with_invalid_timezone_fallback() {
        // test_make_local_datetime_with_invalid_timezone_fallback: unknown zone → UTC.
        let mut ctx = php_ctx();
        ctx.user_tz = "Invalid/Zone".into();
        let r = make_local_datetime(Some(&date_fmt("Y-m-d H:i:s", Utc::now().timestamp())), false, &ctx);
        assert!(!r.is_empty());
    }

    #[test]
    fn make_local_with_automatic_timezone() {
        // test_make_local_datetime_with_automatic_timezone: Automatic + offset 0.
        let mut ctx = php_ctx();
        ctx.user_tz = "Automatic".into();
        ctx.client_tz_offset = 0;
        let r = make_local_datetime(Some(&date_fmt("Y-m-d H:i:s", Utc::now().timestamp())), false, &ctx);
        assert!(!r.is_empty());
    }

    #[test]
    fn make_local_eta_min_with_tz() {
        // test_make_local_datetime_eta_min_with_tz: 15 min ago → "15 min".
        let past = Utc::now().timestamp() - 15 * 60;
        let r = make_local_datetime(Some(&date_fmt("Y-m-d H:i:s", past)), true, &php_ctx());
        assert_eq!(r, "15 min");
    }
}
