//! Port of the `Prefs` registry + per-user resolution (subset used by M2 read
//! ops).
//!
//! In this deployment the DB registry table `ttrss_prefs` is empty — the pref
//! defaults live in the PHP `Prefs::_DEFAULTS` table, which is reproduced here.
//! `Prefs::get(name, uid, profile)` resolves the effective profile (null if the
//! pref is profile-blacklisted or no profile is active), then a
//! `ttrss_user_prefs2` override for `(uid, effective_profile)`, else the PHP
//! default, and casts the string to the pref's type hint (`Config::cast_to`).

use crate::core::config::sql_bool_to_bool;
use crate::core::db::Db;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

/// Pref-name constants (mirror `Prefs::*` in classes/Prefs.php).
pub const ENABLE_FEED_CATS: &str = "ENABLE_FEED_CATS";
pub const USER_TIMEZONE: &str = "USER_TIMEZONE";
pub const DIGEST_MIN_SCORE: &str = "DIGEST_MIN_SCORE";
pub const _ENABLED_PLUGINS: &str = "_ENABLED_PLUGINS";
pub const LONG_DATE_FORMAT: &str = "LONG_DATE_FORMAT";
pub const SHORT_DATE_FORMAT: &str = "SHORT_DATE_FORMAT";
pub const FRESH_ARTICLE_MAX_AGE: &str = "FRESH_ARTICLE_MAX_AGE";
pub const STRIP_IMAGES: &str = "STRIP_IMAGES";
pub const DEFAULT_SEARCH_LANGUAGE: &str = "DEFAULT_SEARCH_LANGUAGE";
pub const VFEED_GROUP_BY_FEED: &str = "VFEED_GROUP_BY_FEED";
pub const PURGE_OLD_DAYS: &str = "PURGE_OLD_DAYS";
pub const PURGE_UNREAD_ARTICLES: &str = "PURGE_UNREAD_ARTICLES";
pub const HEADLINES_NO_DISTINCT: &str = "HEADLINES_NO_DISTINCT";
pub const ENABLE_API_ACCESS: &str = "ENABLE_API_ACCESS";
pub const DEFAULT_UPDATE_INTERVAL: &str = "DEFAULT_UPDATE_INTERVAL";
pub const DIGEST_PREFERRED_TIME: &str = "DIGEST_PREFERRED_TIME";
pub const USER_CSS_THEME: &str = "USER_CSS_THEME";
pub const USER_STYLESHEET: &str = "USER_STYLESHEET";
pub const USER_LANGUAGE: &str = "USER_LANGUAGE";
pub const BLACKLISTED_TAGS: &str = "BLACKLISTED_TAGS";
pub const SSL_CERT_SERIAL: &str = "SSL_CERT_SERIAL";
pub const WIDESCREEN_MODE: &str = "WIDESCREEN_MODE";
/// PHP `Config::T_*` type hints.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// `json_encode(Prefs::get(...))` — the typed value as JSON (bool / int /
    /// string per the pref's `Config::T_*` hint).
    pub fn to_json(&self) -> Value {
        match self {
            Pref::Bool(b) => json!(*b),
            Pref::Int(i) => json!(*i),
            Pref::Str(s) => json!(s.as_str()),
        }
    }
}

/// `Prefs::_PROFILE_BLACKLIST` — prefs that ignore the active profile and
/// always resolve against the default profile.
pub const PROFILE_BLACKLIST: &[&str] = &[
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

/// One row of the PHP `Prefs::_DEFAULTS` table: the pref name, its default
/// value (as written in the PHP source) and its `Config::T_*` type hint.
pub struct PrefDef {
    pub name: &'static str,
    pub default: &'static str,
    pub ty: Type,
}

/// The PHP `Prefs::_DEFAULTS` table, in the PHP constant declaration order
/// (the order `Prefs::_get_all` emits entries).
pub fn all_defs() -> &'static [PrefDef] {
    &[
        PrefDef {
            name: "PURGE_OLD_DAYS",
            default: "60",
            ty: Type::Int,
        },
        PrefDef {
            name: "DEFAULT_UPDATE_INTERVAL",
            default: "30",
            ty: Type::Int,
        },
        PrefDef {
            name: "ENABLE_FEED_CATS",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "SHOW_CONTENT_PREVIEW",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "SHORT_DATE_FORMAT",
            default: "M d, G:i",
            ty: Type::Str,
        },
        PrefDef {
            name: "LONG_DATE_FORMAT",
            default: "D, M d Y - G:i",
            ty: Type::Str,
        },
        PrefDef {
            name: "COMBINED_DISPLAY_MODE",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "HIDE_READ_FEEDS",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "ON_CATCHUP_SHOW_NEXT_FEED",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "FEEDS_SORT_BY_UNREAD",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "REVERSE_HEADLINES",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "DIGEST_ENABLE",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "CONFIRM_FEED_CATCHUP",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "CDM_AUTO_CATCHUP",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "_DEFAULT_VIEW_MODE",
            default: "adaptive",
            ty: Type::Str,
        },
        PrefDef {
            name: "_DEFAULT_VIEW_LIMIT",
            default: "30",
            ty: Type::Int,
        },
        PrefDef {
            name: "BLACKLISTED_TAGS",
            default: "main, generic, misc, uncategorized, blog, blogroll, general, news",
            ty: Type::Str,
        },
        PrefDef {
            name: "FRESH_ARTICLE_MAX_AGE",
            default: "24",
            ty: Type::Int,
        },
        PrefDef {
            name: "RECENTLY_READ_MAX_AGE",
            default: "24",
            ty: Type::Int,
        },
        PrefDef {
            name: "DIGEST_CATCHUP",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "CDM_EXPANDED",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "PURGE_UNREAD_ARTICLES",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "HIDE_READ_SHOWS_SPECIAL",
            default: "true",
            ty: Type::Bool,
        },
        PrefDef {
            name: "VFEED_GROUP_BY_FEED",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "STRIP_IMAGES",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "_DEFAULT_VIEW_ORDER_BY",
            default: "default",
            ty: Type::Str,
        },
        PrefDef {
            name: "ENABLE_API_ACCESS",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "ENABLE_API_ACCESS_BROWSER",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "USER_TIMEZONE",
            default: "Automatic",
            ty: Type::Str,
        },
        PrefDef {
            name: "USER_STYLESHEET",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "SSL_CERT_SERIAL",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "DIGEST_PREFERRED_TIME",
            default: "00:00",
            ty: Type::Str,
        },
        PrefDef {
            name: "_DEFAULT_INCLUDE_CHILDREN",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "_ENABLED_PLUGINS",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "USER_CSS_THEME",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "USER_LANGUAGE",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "DEFAULT_SEARCH_LANGUAGE",
            default: "",
            ty: Type::Str,
        },
        PrefDef {
            name: "_PREFS_MIGRATED",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "HEADLINES_NO_DISTINCT",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "DEBUG_HEADLINE_IDS",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "DISABLE_CONDITIONAL_COUNTERS",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "WIDESCREEN_MODE",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "CDM_ENABLE_GRID",
            default: "false",
            ty: Type::Bool,
        },
        PrefDef {
            name: "DIGEST_MIN_SCORE",
            default: "0",
            ty: Type::Int,
        },
    ]
}

/// The PHP `Prefs::_DEFAULTS` lookup: (default, type hint) for a known pref.
pub fn default_for(name: &str) -> Option<(&'static str, Type)> {
    all_defs()
        .iter()
        .find(|d| d.name == name)
        .map(|d| (d.default, d.ty))
}

/// PHP `(int)` cast: optional leading whitespace + sign, then decimal
/// digits only (stops at the first non-digit; no fraction/exponent/hex).
/// `(int) "  -12.5"` → -12, `(int) "0x10"` → 0, `(int) ""` → 0.
fn php_int(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        neg = b[i] == b'-';
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if start == i {
        return 0;
    }
    let mut v = 0i64;
    for d in &b[start..i] {
        v = v
            .checked_mul(10)
            .and_then(|v| v.checked_add((d - b'0') as i64))
            .unwrap_or(0);
    }
    if neg {
        -v
    } else {
        v
    }
}

/// `Config::cast_to(value, type_hint)`.
fn cast(value: &str, ty: Type) -> Pref {
    match ty {
        // T_BOOL cast: `Config::cast_to` → `sql_bool_to_bool` (core).
        Type::Bool => Pref::Bool(sql_bool_to_bool(value)),
        Type::Int => Pref::Int(php_int(value)),
        Type::Str => Pref::Str(value.to_string()),
    }
}

/// How a resolved value is written to `ttrss_user_prefs2.value` (text):
/// PHP 8.1+ PDO binds `true` as `"1"` / `false` as `""` (the string form),
/// ints as their decimal string, and strings as-is (see `PrefsTest::
/// test_set_stores_bool_as_string_in_db`).
fn stored_string(v: &Pref) -> String {
    match v {
        Pref::Bool(true) => "1".into(),
        Pref::Bool(false) => String::new(),
        Pref::Int(i) => i.to_string(),
        Pref::Str(s) => s.clone(),
    }
}

/// Load a user's pref overrides for the given profile **and** the default
/// (null) profile in one query (mirrors `Prefs::cache_all`).
///
/// Returns `(pref_name, profile, value)` rows.
pub async fn load_rows(
    db: &Db,
    uid: i64,
    profile: Option<i32>,
) -> Vec<(String, Option<i32>, String)> {
    load_rows_pool(db.pool(), uid, profile).await
}

/// `load_rows` for a bare pool (no `Db` wrapper) — the update pipeline (M7.9)
/// has only a `PgPool` to read the owner's enabled plugins from.
pub async fn load_rows_pool(
    pool: &PgPool,
    uid: i64,
    profile: Option<i32>,
) -> Vec<(String, Option<i32>, String)> {
    let rows = sqlx::query(
        "SELECT pref_name, profile, value FROM ttrss_user_prefs2
         WHERE owner_uid = $1 AND (profile = $2 OR profile IS NULL)",
    )
    .bind(uid)
    .bind(profile)
    .fetch_all(pool)
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

/// `load_rows` with a per-process cache (the PHP's `Prefs` singleton `$cache`):
/// one `SELECT` per (uid, profile), then memory hits until invalidated.
pub async fn load_rows_cached(
    cache: &crate::PrefsCache,
    db: &Db,
    uid: i64,
    profile: Option<i32>,
) -> Vec<(String, Option<i32>, String)> {
    // Fast path: cache hit.
    if let Some(rows) = cache.read().ok().and_then(|c| c.get(&(uid, profile)).cloned()) {
        return rows;
    }
    // Slow path: query + populate.
    let rows = load_rows(db, uid, profile).await;
    if let Ok(mut c) = cache.write() {
        c.insert((uid, profile), rows.clone());
    }
    rows
}

/// Drop the cached rows for a (uid, profile) — called after a prefs save so
/// the next read reflects the change (the PHP's `Prefs::set` updates the
/// in-memory cache entry).
pub fn invalidate(cache: &crate::PrefsCache, uid: i64, profile: Option<i32>) {
    if let Ok(mut c) = cache.write() {
        c.remove(&(uid, profile));
    }
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
        n == name
            && match (effective, *p) {
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

/// `Prefs::set` (`_set`): optional strip_tags + trim, cast to the pref's type hint,
/// no-op when the value already equals the effective value, else upsert the
/// override row. Returns `false` (no write) for unknown pref names and for
/// profile-blacklisted prefs with a profile, matching the PHP `user_error`
/// / early-return paths.
pub async fn set(
    db: &Db,
    name: &str,
    value: &str,
    owner_uid: i64,
    profile: Option<i32>,
    strip_tags: bool,
) -> bool {
    // PHP: `if (!$profile_id) $profile_id = null;`
    let profile = if profile == Some(0) { None } else { profile };
    if profile.is_some() && PROFILE_BLACKLIST.contains(&name) {
        return false;
    }
    let Some(def) = all_defs().iter().find(|d| d.name == name) else {
        // PHP: `user_error("Attempt to set invalid preference key …")`.
        return false;
    };
    let value = if strip_tags {
        cast(crate::util::strip_tags(value).trim(), def.ty)
    } else {
        cast(value, def.ty)
    };

    // The `_get` the no-op check compares against: the (uid, profile)
    // override if present, else the PHP default.
    let cur_raw = sqlx::query_scalar::<_, String>(
        "SELECT value FROM ttrss_user_prefs2
         WHERE pref_name = $1 AND owner_uid = $2
           AND (profile = $3 OR ($3 IS NULL AND profile IS NULL))",
    )
    .bind(name)
    .bind(owner_uid)
    .bind(profile)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    let current = match cur_raw.as_ref() {
        Some(raw) => cast(raw, def.ty),
        None => cast(def.default, def.ty),
    };
    if value == current {
        return true; // "no need to actually set this to the same value"
    }

    let stored = stored_string(&value);
    if cur_raw.is_some() {
        sqlx::query(
            "UPDATE ttrss_user_prefs2 SET value = $1
             WHERE pref_name = $2 AND owner_uid = $3
               AND (profile = $4 OR ($4 IS NULL AND profile IS NULL))",
        )
        .bind(&stored)
        .bind(name)
        .bind(owner_uid)
        .bind(profile)
        .execute(db.pool())
        .await
        .is_ok()
    } else {
        sqlx::query(
            "INSERT INTO ttrss_user_prefs2 (pref_name, value, owner_uid, profile)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(name)
        .bind(&stored)
        .bind(owner_uid)
        .bind(profile)
        .execute(db.pool())
        .await
        .is_ok()
    }
}

/// `Prefs::reset`: delete all of the user's override rows for the profile
/// except `_PREFS_MIGRATED`.
pub async fn reset(db: &Db, owner_uid: i64, profile: Option<i32>) {
    let profile = if profile == Some(0) { None } else { profile };
    let _ = sqlx::query(
        "DELETE FROM ttrss_user_prefs2
         WHERE owner_uid = $1 AND pref_name != '_PREFS_MIGRATED'
           AND (profile = $2 OR ($2 IS NULL AND profile IS NULL))",
    )
    .bind(owner_uid)
    .bind(profile)
    .execute(db.pool())
    .await;
}

/// `Prefs::get_all`: every pref in `_DEFAULTS` (declaration order) with the
/// resolved value for (uid, profile) and its `Config::T_*` type hint.
/// `rows` is the output of [`load_rows`].
pub fn get_all(
    rows: &[(String, Option<i32>, String)],
    profile: Option<i32>,
) -> Vec<(String, Pref, Type)> {
    all_defs()
        .iter()
        .map(|d| (d.name.to_string(), resolve(d.name, rows, profile), d.ty))
        .collect()
}

/// `Prefs::get_profile_id`: the id of the user's profile with that title,
/// or -1 if none exists.
pub async fn get_profile_id(db: &Db, owner_uid: i64, title: &str) -> i64 {
    let id: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_settings_profiles WHERE owner_uid = $1 AND title = $2",
    )
    .bind(owner_uid)
    .bind(title)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    id.map(|v| v as i64).unwrap_or(-1)
}

/// `Prefs::profile_exists`.
pub async fn profile_exists(db: &Db, owner_uid: i64, profile_id: i64) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT id FROM ttrss_settings_profiles WHERE owner_uid = $1 AND id = $2",
    )
    .bind(owner_uid)
    .bind(profile_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten()
    .is_some()
}

/// `Prefs::create_profile`: the id of the created profile; -1 if the title
/// is already used by the user or the insert fails.
pub async fn create_profile(db: &Db, owner_uid: i64, title: &str) -> i64 {
    if get_profile_id(db, owner_uid, title).await != -1 {
        return -1;
    }
    match sqlx::query_scalar::<_, i32>(
        "INSERT INTO ttrss_settings_profiles (owner_uid, title) VALUES ($1, $2) RETURNING id",
    )
    .bind(owner_uid)
    .bind(title)
    .fetch_optional(db.pool())
    .await
    {
        Ok(Some(id)) => id as i64,
        _ => -1,
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
        assert_eq!(resolve("PURGE_OLD_DAYS", &[], None), Pref::Int(60));
        assert_eq!(resolve("DEFAULT_UPDATE_INTERVAL", &[], None), Pref::Int(30));
        assert_eq!(resolve("ENABLE_FEED_CATS", &[], None), Pref::Bool(true));
        assert_eq!(resolve("REVERSE_HEADLINES", &[], None), Pref::Bool(false));
        assert_eq!(
            resolve("SHORT_DATE_FORMAT", &[], None),
            Pref::Str("M d, G:i".into())
        );
        assert_eq!(
            resolve("_DEFAULT_VIEW_MODE", &[], None),
            Pref::Str("adaptive".into())
        );
    }

    #[test]
    fn get_default_unknown_pref() {
        // test_get_default_returns_null_for_unknown_pref
        assert_eq!(
            resolve("NONEXISTENT_PREF", &[], None),
            Pref::Str(String::new())
        );
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
        assert_eq!(
            resolve("SHORT_DATE_FORMAT", &[], None),
            Pref::Str("M d, G:i".into())
        );
        assert_eq!(
            resolve("USER_TIMEZONE", &[], None),
            Pref::Str("Automatic".into())
        );
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
        assert_eq!(
            resolve("SHORT_DATE_FORMAT", &rows, None),
            Pref::Str(String::new())
        );
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
        assert_eq!(
            resolve("REVERSE_HEADLINES", &rows, Some(5)),
            Pref::Bool(true)
        );
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
        assert_eq!(
            resolve("REVERSE_HEADLINES", &rows, Some(5)),
            Pref::Bool(true)
        );
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

    // ── DB-dependent PrefsTest.php cases ──────────────────────────────────────
    // Ported 1:1 in `tests/prefs_parity.rs` (the 14 DB-backed cases this
    // block used to park): `Prefs::set` / `reset` / `get_all` /
    // `create_profile` are now implemented above.
}
