//! Port of the `Prefs` registry + per-user resolution (subset used by M2 read
//! ops).
//!
//! In this deployment the DB registry table `ttrss_prefs` is empty — the pref
//! defaults live in the PHP `Prefs::_DEFAULTS` table, which is reproduced here.
//! `Prefs::get(name, uid, profile)` resolves the effective profile (null if the
//! pref is profile-blacklisted or no profile is active), then a
//! `ttrss_user_prefs2` override for `(uid, effective_profile)`, else the PHP
//! default, and casts the string to the pref's type hint (`Config::cast_to`).

use sqlx::Row;
use ttrss_core::config::sql_bool_to_bool;
use ttrss_core::db::Db;

/// PHP `Config::T_*` type hints.
#[derive(Clone, Copy, PartialEq)]
pub enum Type {
    Bool, // Config::T_BOOL
    Str,  // Config::T_STRING
    Int,  // Config::T_INT
}

/// A resolved pref value (already cast to its type hint).
#[derive(Clone, Debug, PartialEq)]
pub enum Pref {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl Pref {
    /// `(int) Prefs::get(...)` — the int cast the init-params builder applies.
    pub fn as_int(&self) -> i64 {
        match self {
            Pref::Bool(b) => *b as i64,
            Pref::Int(i) => *i,
            Pref::Str(s) => s.trim().parse().unwrap_or(0),
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            Pref::Bool(b) => *b,
            Pref::Int(i) => *i != 0,
            Pref::Str(s) => sql_bool_to_bool(s),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Pref::Str(s) => s,
            Pref::Int(i) => i.to_string().leak(),
            Pref::Bool(b) => b.to_string().leak(),
        }
    }
}

/// `Prefs::_PROFILE_BLACKLIST` — prefs that ignore the active profile and
/// always resolve against the default profile.
const PROFILE_BLACKLIST: &[&str] = &[
    "PURGE_OLD_DAYS",
    "PURGE_UNREAD_ARTICLES",
    "DIGEST_ENABLE",
    "DIGEST_CATCHUP",
    "BLACKLISTED_TAGS",
    "ENABLE_API_ACCESS",
    "ENABLE_API_ACCESS_BROWSER",
    "DEFAULT_UPDATE_INTERVAL",
    "USER_TIMEZONE",
    "SSL_CERT_SERIAL",
    "DIGEST_PREFERRED_TIME",
    "DIGEST_MIN_SCORE",
    "_PREFS_MIGRATED",
];

/// The PHP `Prefs::_DEFAULTS` table: name -> (default, type hint).
pub fn default_for(name: &str) -> Option<(&'static str, Type)> {
    Some(match name {
        "PURGE_OLD_DAYS" => ("60", Type::Int),
        "DEFAULT_UPDATE_INTERVAL" => ("30", Type::Int),
        "ENABLE_FEED_CATS" => ("true", Type::Bool),
        "SHOW_CONTENT_PREVIEW" => ("true", Type::Bool),
        "SHORT_DATE_FORMAT" => ("M d, G:i", Type::Str),
        "LONG_DATE_FORMAT" => ("D, M d Y - G:i", Type::Str),
        "COMBINED_DISPLAY_MODE" => ("true", Type::Bool),
        "HIDE_READ_FEEDS" => ("false", Type::Bool),
        "ON_CATCHUP_SHOW_NEXT_FEED" => ("false", Type::Bool),
        "FEEDS_SORT_BY_UNREAD" => ("false", Type::Bool),
        "REVERSE_HEADLINES" => ("false", Type::Bool),
        "DIGEST_ENABLE" => ("false", Type::Bool),
        "CONFIRM_FEED_CATCHUP" => ("true", Type::Bool),
        "CDM_AUTO_CATCHUP" => ("false", Type::Bool),
        "_DEFAULT_VIEW_MODE" => ("adaptive", Type::Str),
        "_DEFAULT_VIEW_LIMIT" => ("30", Type::Int),
        "BLACKLISTED_TAGS" => (
            "main, generic, misc, uncategorized, blog, blogroll, general, news",
            Type::Str,
        ),
        "FRESH_ARTICLE_MAX_AGE" => ("24", Type::Int),
        "RECENTLY_READ_MAX_AGE" => ("24", Type::Int),
        "DIGEST_CATCHUP" => ("false", Type::Bool),
        "CDM_EXPANDED" => ("true", Type::Bool),
        "PURGE_UNREAD_ARTICLES" => ("true", Type::Bool),
        "HIDE_READ_SHOWS_SPECIAL" => ("true", Type::Bool),
        "VFEED_GROUP_BY_FEED" => ("false", Type::Bool),
        "STRIP_IMAGES" => ("false", Type::Bool),
        "_DEFAULT_VIEW_ORDER_BY" => ("default", Type::Str),
        "ENABLE_API_ACCESS" => ("false", Type::Bool),
        "ENABLE_API_ACCESS_BROWSER" => ("false", Type::Bool),
        "USER_TIMEZONE" => ("Automatic", Type::Str),
        "USER_STYLESHEET" => ("", Type::Str),
        "SSL_CERT_SERIAL" => ("", Type::Str),
        "DIGEST_PREFERRED_TIME" => ("00:00", Type::Str),
        "_DEFAULT_INCLUDE_CHILDREN" => ("false", Type::Bool),
        "_ENABLED_PLUGINS" => ("", Type::Str),
        "USER_CSS_THEME" => ("", Type::Str),
        "USER_LANGUAGE" => ("", Type::Str),
        "DEFAULT_SEARCH_LANGUAGE" => ("", Type::Str),
        "_PREFS_MIGRATED" => ("false", Type::Bool),
        "HEADLINES_NO_DISTINCT" => ("false", Type::Bool),
        "DEBUG_HEADLINE_IDS" => ("false", Type::Bool),
        "DISABLE_CONDITIONAL_COUNTERS" => ("false", Type::Bool),
        "WIDESCREEN_MODE" => ("false", Type::Bool),
        "CDM_ENABLE_GRID" => ("false", Type::Bool),
        "DIGEST_MIN_SCORE" => ("0", Type::Int),
        _ => return None,
    })
}

/// `Config::cast_to(value, type_hint)`.
fn cast(value: &str, ty: Type) -> Pref {
    match ty {
        // T_BOOL cast: `Config::cast_to` → `sql_bool_to_bool` (core).
        Type::Bool => Pref::Bool(sql_bool_to_bool(value)),
        Type::Int => Pref::Int(value.trim().parse().unwrap_or(0)),
        Type::Str => Pref::Str(value.to_string()),
    }
}

/// Load a user's pref overrides for the given profile **and** the default
/// (null) profile in one query (mirrors `Prefs::cache_all`).
///
/// Returns `(pref_name, profile, value)` rows.
pub async fn load_rows(db: &Db, uid: i64, profile: Option<i32>) -> Vec<(String, Option<i32>, String)> {
    let rows = sqlx::query(
        "SELECT pref_name, profile, value FROM ttrss_user_prefs2
         WHERE owner_uid = $1 AND (profile = $2 OR profile IS NULL)",
    )
    .bind(uid)
    .bind(profile)
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            Some((
                r.get::<String, _>(0),
                r.try_get::<Option<i32>, _>(1).ok()?,
                r.get::<String, _>(2),
            ))
        })
        .collect()
}

/// Resolve a pref: (uid, profile) override -> profile-blacklist default ->
/// PHP default, then cast to the type hint.
pub fn resolve(name: &str, rows: &[(String, Option<i32>, String)], profile: Option<i32>) -> Pref {
    let Some((def, ty)) = default_for(name) else {
        return Pref::Str(String::new());
    };
    // `!$profile_id || in_array($pref_name, BLACKLIST)` -> use default profile.
    let effective = if profile.unwrap_or(0) == 0 || PROFILE_BLACKLIST.contains(&name) {
        None
    } else {
        profile
    };
    if let Some((_, p, value)) = rows.iter().find(|(n, p, _)| {
        n == name && match (effective, *p) {
            (Some(e), Some(a)) => e == a,
            (None, None) => true,
            _ => false,
        }
    }) {
        let _ = p;
        cast(value, ty)
    } else {
        cast(def, ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Prefs::is_valid` — the pref is known iff `default_for` has a row.
    fn is_valid(name: &str) -> bool {
        default_for(name).is_some()
    }

    fn row(name: &str, profile: Option<i32>, value: &str) -> (String, Option<i32>, String) {
        (name.to_string(), profile, value.to_string())
    }

    // ── is_valid ────────────────────────────────────────────────────────────

    #[test]
    fn is_valid_known_prefs() {
        // test_is_valid_returns_true_for_known_prefs
        assert!(is_valid("PURGE_OLD_DAYS"));
        assert!(is_valid("ENABLE_FEED_CATS"));
        assert!(is_valid("USER_TIMEZONE"));
        assert!(is_valid("USER_LANGUAGE"));
    }

    #[test]
    fn is_valid_unknown_prefs() {
        // test_is_valid_returns_false_for_unknown_prefs
        assert!(!is_valid("NONEXISTENT_PREF"));
        assert!(!is_valid(""));
        assert!(!is_valid("some_random_key"));
    }

    // ── get_default ─────────────────────────────────────────────────────────

    #[test]
    fn get_default_values() {
        // test_get_default_returns_default_values
        assert_eq!(
            resolve("PURGE_OLD_DAYS", &[], None),
            Pref::Int(60)
        );
        assert_eq!(resolve("DEFAULT_UPDATE_INTERVAL", &[], None), Pref::Int(30));
        assert_eq!(resolve("ENABLE_FEED_CATS", &[], None), Pref::Bool(true));
        assert_eq!(resolve("REVERSE_HEADLINES", &[], None), Pref::Bool(false));
        assert_eq!(resolve("SHORT_DATE_FORMAT", &[], None), Pref::Str("M d, G:i".into()));
        assert_eq!(resolve("_DEFAULT_VIEW_MODE", &[], None), Pref::Str("adaptive".into()));
    }

    #[test]
    fn get_default_unknown_pref() {
        // test_get_default_returns_null_for_unknown_pref
        assert_eq!(resolve("NONEXISTENT_PREF", &[], None), Pref::Str(String::new()));
    }

    // ── get — defaults (no DB row) ──────────────────────────────────────────

    #[test]
    fn get_returns_default_when_no_db_row() {
        // test_get_returns_default_when_no_db_row_exists
        assert_eq!(resolve("PURGE_OLD_DAYS", &[], None), Pref::Int(60));
    }

    #[test]
    fn get_correct_types_int_prefs() {
        // test_get_returns_correct_types_for_int_prefs
        assert_eq!(resolve("PURGE_OLD_DAYS", &[], None), Pref::Int(60));
        assert_eq!(resolve("DEFAULT_UPDATE_INTERVAL", &[], None), Pref::Int(30));
    }

    #[test]
    fn get_correct_types_bool_prefs() {
        // test_get_returns_correct_types_for_bool_prefs
        assert_eq!(resolve("ENABLE_FEED_CATS", &[], None), Pref::Bool(true));
        assert_eq!(resolve("REVERSE_HEADLINES", &[], None), Pref::Bool(false));
    }

    #[test]
    fn get_correct_types_string_prefs() {
        // test_get_returns_correct_types_for_string_prefs
        assert_eq!(resolve("SHORT_DATE_FORMAT", &[], None), Pref::Str("M d, G:i".into()));
        assert_eq!(resolve("USER_TIMEZONE", &[], None), Pref::Str("Automatic".into()));
    }

    // ── get — from database (override rows) ─────────────────────────────────

    #[test]
    fn get_returns_overridden_value_from_db() {
        // test_get_returns_overridden_value_from_db
        let rows = vec![row("PURGE_OLD_DAYS", None, "90")];
        assert_eq!(resolve("PURGE_OLD_DAYS", &rows, None), Pref::Int(90));
    }

    #[test]
    fn get_with_empty_string_value_for_string_pref() {
        // test_get_with_empty_string_value_for_string_pref
        let rows = vec![row("SHORT_DATE_FORMAT", None, "")];
        assert_eq!(resolve("SHORT_DATE_FORMAT", &rows, None), Pref::Str(String::new()));
    }

    // ── cast ────────────────────────────────────────────────────────────────

    #[test]
    fn cast_string_to_int() {
        // test_set_casts_string_to_int_for_int_prefs (the get side)
        let rows = vec![row("PURGE_OLD_DAYS", None, "45")];
        assert_eq!(resolve("PURGE_OLD_DAYS", &rows, None), Pref::Int(45));
    }

    #[test]
    fn cast_string_to_bool() {
        // test_set_casts_string_to_bool_for_bool_prefs (the get side)
        let rows = vec![row("ENABLE_FEED_CATS", None, "f")];
        assert_eq!(resolve("ENABLE_FEED_CATS", &rows, None), Pref::Bool(false));
        let rows = vec![row("ENABLE_FEED_CATS", None, "1")];
        assert_eq!(resolve("ENABLE_FEED_CATS", &rows, None), Pref::Bool(true));
    }

    // ── profile handling ────────────────────────────────────────────────────

    #[test]
    fn get_ignores_profile_for_blacklisted_pref() {
        // test_get_ignores_profile_for_blacklisted_pref: a profile-specific row
        // for a blacklisted pref is ignored (falls back to user-level value).
        let rows = vec![
            row("PURGE_OLD_DAYS", None, "500"),
            row("PURGE_OLD_DAYS", Some(300), "777"),
        ];
        assert_eq!(resolve("PURGE_OLD_DAYS", &rows, Some(300)), Pref::Int(500));
    }

    #[test]
    fn set_and_get_profile_specific_pref() {
        // test_set_and_get_profile_specific_pref: a non-blacklisted pref with a
        // profile resolves to the profile-specific row.
        let rows = vec![row("REVERSE_HEADLINES", Some(5), "1")];
        assert_eq!(resolve("REVERSE_HEADLINES", &rows, Some(5)), Pref::Bool(true));
    }

    #[test]
    fn profile_pref_does_not_affect_user_level_pref() {
        // test_profile_pref_does_not_affect_user_level_pref: user-level (no
        // profile) vs profile-level overrides are independent. Prefs::set stores
        // false as "" and true as "1".
        let rows = vec![
            row("REVERSE_HEADLINES", None, ""),
            row("REVERSE_HEADLINES", Some(5), "1"),
        ];
        assert_eq!(resolve("REVERSE_HEADLINES", &rows, None), Pref::Bool(false));
        assert_eq!(resolve("REVERSE_HEADLINES", &rows, Some(5)), Pref::Bool(true));
    }

    #[test]
    fn cast_bool_zero_string_is_false() {
        // sql_bool_to_bool("0") === false (PHP string "0" is falsy). Guards the
        // regression where a "0"/"" override was misread as true.
        let rows = vec![row("REVERSE_HEADLINES", None, "0")];
        assert_eq!(resolve("REVERSE_HEADLINES", &rows, None), Pref::Bool(false));
        let rows = vec![row("REVERSE_HEADLINES", None, "")];
        assert_eq!(resolve("REVERSE_HEADLINES", &rows, None), Pref::Bool(false));
    }

    // ── DB-dependent PrefsTest.php cases (REVISIT) ────────────────────────────
    // These exercise Prefs::set / reset / get_all (DB writes/reads), which the
    // Rust `prefs` module does not (yet) implement — it only resolves against
    // pre-loaded rows. Kept here (not dropped) so they are revisited when a
    // DB-backed `set`/`reset`/`get_all` is ported. Tracked in docs/plan.md
    // (test-parity, prefs.rs row):
    //   test_set_inserts_new_row_into_db
    //   test_set_updates_existing_row
    //   test_set_stores_bool_as_string_in_db
    //   test_set_stores_string_in_db
    //   test_set_strips_tags_from_string_values
    //   test_set_returns_true_without_db_change_when_value_unchanged
    //   test_get_all_returns_all_pref_definitions
    //   test_get_all_returns_default_values_for_new_user
    //   test_set_profile_blacklisted_pref_returns_false
    //   test_set_non_blacklisted_pref_works_with_profile
    //   test_reset_clears_all_user_prefs
    //   test_reset_does_not_delete_migrated_flag
    //   test_set_returns_false_for_invalid_pref_name
    //   test_multiple_users_have_isolated_prefs
}
