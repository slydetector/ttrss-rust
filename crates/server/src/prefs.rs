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
use ttrss_core::db::Db;

/// PHP `Config::T_*` type hints.
#[derive(Clone, Copy, PartialEq)]
pub enum Type {
    Bool, // Config::T_BOOL
    Str,  // Config::T_STRING
    Int,  // Config::T_INT
}

/// A resolved pref value (already cast to its type hint).
#[derive(Clone, Debug)]
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
            Pref::Str(s) => !s.is_empty() && s != "f" && s != "false" && s != "0",
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
        // sql_bool_to_bool: truthy unless empty or "f"/"false".
        Type::Bool => Pref::Bool(!value.is_empty() && value != "f" && value != "false"),
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
