//! M10.1 — i18n substrate: catalog loading + locale resolution.
//!
//! PHP reference: `include/functions.php` (`startup_gettext`) + `lib/gettext/`.
//! The locale is resolved per-request: `USER_LANGUAGE` pref wins; fallback to
//! `Accept-Language` header; fallback to English (identity).
//!
//! Catalogs (`.mo` files) are loaded lazily on first use per locale.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gettext::Catalog;

/// A lazily-populated cache of gettext catalogs, keyed by locale.
///
/// Each locale's `.mo` file is parsed once on first access; subsequent
/// lookups reuse the cached `Arc<Catalog>`.
pub struct I18nCache {
    /// The docroot `locale/` directory (contains `<locale>/LC_MESSAGES/messages.mo`).
    locale_dir: PathBuf,
    /// Locale → parsed catalog. Inserted on first use.
    catalogs: Mutex<HashMap<String, Arc<Catalog>>>,
    /// The set of available locales (directory names under `locale_dir/`).
    available_locales: Vec<String>,
}

impl I18nCache {
    /// Create a new cache pointing at the given `locale/` directory.
    /// Scans the directory to determine available locales.
    pub fn new(locale_dir: impl Into<PathBuf>) -> Self {
        let locale_dir = locale_dir.into();
        let mut available_locales = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&locale_dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                // A locale dir must contain LC_MESSAGES/messages.mo
                if path.is_dir()
                    && path.join("LC_MESSAGES").join("messages.mo").is_file()
                {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        available_locales.push(name.to_string());
                    }
                }
            }
        }
        available_locales.sort();
        Self {
            locale_dir,
            catalogs: Mutex::new(HashMap::new()),
            available_locales,
        }
    }

    /// The list of available locale codes (e.g., `["de_DE", "fr_FR", ...]`).
    pub fn available_locales(&self) -> &[String] {
        &self.available_locales
    }

    /// Get the catalog for a locale, loading it lazily on first access.
    /// Returns `None` if the locale is unknown or the file can't be parsed.
    pub fn catalog(&self, locale: &str) -> Option<Arc<Catalog>> {
        // Fast path: already loaded.
        {
            let map = self.catalogs.lock().unwrap();
            if let Some(cat) = map.get(locale) {
                return Some(Arc::clone(cat));
            }
        }
        // Slow path: load and insert.
        let path = self
            .locale_dir
            .join(locale)
            .join("LC_MESSAGES")
            .join("messages.mo");
        let file = std::fs::File::open(&path).ok()?;
        let catalog = Catalog::parse(file).ok()?;
        let arc = Arc::new(catalog);
        let mut map = self.catalogs.lock().unwrap();
        // Double-check (another thread may have loaded it).
        if let Some(existing) = map.get(locale) {
            return Some(Arc::clone(existing));
        }
        map.insert(locale.to_string(), Arc::clone(&arc));
        Some(arc)
    }
}

/// Resolve the user's locale, matching PHP's `startup_gettext()`:
/// 1. If `user_pref` is set and not `"auto"` → use it.
/// 2. Else parse `Accept-Language` (q-factors, full-locale-then-2-letter
///    match against available locales).
/// 3. Else `None` (English / identity).
pub fn resolve_locale(
    accept_language: Option<&str>,
    user_pref: Option<&str>,
    available: &[String],
) -> Option<String> {
    // 1. User preference wins (unless "auto").
    if let Some(pref) = user_pref {
        if !pref.is_empty() && pref != "auto" {
            if available.iter().any(|a| a == pref) {
                return Some(pref.to_string());
            }
        }
    }

    // 2. Accept-Language header.
    let header = accept_language?;
    // The crate returns languages sorted by q-factor (highest first).
    let parsed: Vec<String> = accept_language::parse(header);

    // Iterate user languages in q-factor order; for each, try full match
    // then 2-letter prefix against available locales.
    for lang in &parsed {
        let lang_lower = lang.to_lowercase();
        // Try full match: normalize both sides (lowercase, _ → -).
        for locale in available {
            let normalized = locale.to_lowercase().replace('_', "-");
            if normalized == lang_lower {
                return Some(locale.clone());
            }
        }
        // Try 2-letter prefix.
        let short = &lang_lower[..lang_lower.len().min(2)];
        for locale in available {
            let normalized = locale.to_lowercase();
            if normalized.starts_with(short) {
                return Some(locale.clone());
            }
        }
    }

    None
}

/// Translate `msg` using the catalog for `locale`. Returns the original
/// string if no locale is set, the locale is unknown, or no translation
/// exists (matching PHP's gettext fallback behavior).
pub fn __(locale: Option<&str>, cache: &I18nCache, msg: &str) -> String {
    let Some(locale) = locale else {
        return msg.to_string();
    };
    let Some(catalog) = cache.catalog(locale) else {
        return msg.to_string();
    };
    catalog.gettext(msg).to_string()
}

/// Translate and format: `vsprintf(__(fmt), args)`.
/// Matches PHP's `T_sprintf()`.
pub fn t_sprintf(locale: Option<&str>, cache: &I18nCache, fmt: &str, args: &[&str]) -> String {
    let translated = __(locale, cache, fmt);
    // Replace the first %s or %d in order of appearance with each arg.
    let mut result = translated;
    for arg in args {
        let s_pos = result.find("%s").unwrap_or(usize::MAX);
        let d_pos = result.find("%d").unwrap_or(usize::MAX);
        let pos = s_pos.min(d_pos);
        if pos == usize::MAX {
            break;
        }
        let fmt_char = if s_pos <= d_pos { "%s" } else { "%d" };
        result = result.replacen(fmt_char, arg, 1);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache() -> I18nCache {
        I18nCache::new(PathBuf::from("../../tt-rss/locale"))
    }

    #[test]
    fn test_available_locales() {
        let cache = test_cache();
        let locales = cache.available_locales();
        assert!(!locales.is_empty(), "should find locales");
        assert!(locales.contains(&"de_DE".to_string()), "de_DE should be available");
        assert!(locales.contains(&"fr_FR".to_string()), "fr_FR should be available");
    }

    #[test]
    fn test_catalog_loads_lazily() {
        let cache = test_cache();
        let cat = cache.catalog("de_DE").expect("de_DE catalog");
        let translated = cat.gettext("Loading, please wait...");
        assert_ne!(translated, "Loading, please wait...", "should be translated");
    }

    #[test]
    fn test_unknown_locale_returns_none() {
        let cache = test_cache();
        assert!(cache.catalog("xx_XX").is_none());
    }

    #[test]
    fn test_resolve_locale_pref_wins() {
        let available = vec!["de_DE".into(), "fr_FR".into(), "en_US".into()];
        assert_eq!(
            resolve_locale(Some("fr-FR"), Some("de_DE"), &available).as_deref(),
            Some("de_DE")
        );
    }

    #[test]
    fn test_resolve_locale_pref_auto_falls_through() {
        let available = vec!["de_DE".into(), "fr_FR".into(), "en_US".into()];
        assert_eq!(
            resolve_locale(Some("fr-FR"), Some("auto"), &available).as_deref(),
            Some("fr_FR")
        );
    }

    #[test]
    fn test_resolve_locale_accept_language_full_match() {
        let available = vec!["de_DE".into(), "fr_FR".into(), "en_US".into()];
        assert_eq!(
            resolve_locale(Some("de-DE"), None, &available).as_deref(),
            Some("de_DE")
        );
    }

    #[test]
    fn test_resolve_locale_accept_language_2letter() {
        let available = vec!["de_DE".into(), "fr_FR".into(), "en_US".into()];
        assert_eq!(
            resolve_locale(Some("fr"), None, &available).as_deref(),
            Some("fr_FR")
        );
    }

    #[test]
    fn test_resolve_locale_q_factors() {
        let available = vec!["de_DE".into(), "fr_FR".into(), "en_US".into()];
        // fr has higher q-factor
        assert_eq!(
            resolve_locale(Some("de;q=0.5, fr;q=0.9"), None, &available).as_deref(),
            Some("fr_FR")
        );
    }

    #[test]
    fn test_resolve_locale_no_match() {
        let available = vec!["de_DE".into(), "fr_FR".into()];
        assert_eq!(resolve_locale(Some("ja"), None, &available), None);
    }

    #[test]
    fn test_resolve_locale_no_header_no_pref() {
        let available = vec!["de_DE".into()];
        assert_eq!(resolve_locale(None, None, &available), None);
    }

    #[test]
    fn test_translate_identity_for_no_locale() {
        let cache = test_cache();
        let result = __(None, &cache, "Hello");
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_translate_with_locale() {
        let cache = test_cache();
        let result = __(Some("de_DE"), &cache, "Loading, please wait...");
        assert_ne!(result, "Loading, please wait...");
    }

    #[test]
    fn test_translate_missing_string_falls_back() {
        let cache = test_cache();
        let msg = "This string definitely does not exist in any catalog xyzzy 12345";
        let result = __(Some("de_DE"), &cache, msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_t_sprintf() {
        let cache = test_cache();
        let result = t_sprintf(None, &cache, "Hello %s, you have %d items", &["world", "5"]);
        assert_eq!(result, "Hello world, you have 5 items");
    }
}
