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
    /// Locale → (msgid, msgstr) pairs extracted from the `.mo` file.
    entries: Mutex<HashMap<String, Vec<(String, String)>>>,
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
                    && let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        available_locales.push(name.to_string());
                    }
            }
        }
        available_locales.sort();
        Self {
            locale_dir,
            catalogs: Mutex::new(HashMap::new()),
            available_locales,
            entries: Mutex::new(HashMap::new()),
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

    /// Build the translations map for a locale, matching PHP's
    /// `_translations_as_array()`. Returns `None` if the locale is unknown.
    ///
    /// For singular entries: `{ msgid: msgstr }`.
    /// For plural entries (msgid contains `\0`): both singular and plural forms
    /// are included as separate keys.
    pub fn translations_map(&self, locale: &str) -> Option<Vec<(String, String)>> {
        // Fast path: already loaded.
        {
            let map = self.entries.lock().unwrap();
            if let Some(entries) = map.get(locale) {
                return Some(build_translations(entries));
            }
        }
        // Slow path: parse the .mo file.
        let path = self
            .locale_dir
            .join(locale)
            .join("LC_MESSAGES")
            .join("messages.mo");
        let data = std::fs::read(&path).ok()?;
        let raw = parse_mo_entries(&data)?;
        let mut map = self.entries.lock().unwrap();
        if let Some(existing) = map.get(locale) {
            return Some(build_translations(existing));
        }
        map.insert(locale.to_string(), raw.clone());
        Some(build_translations(&raw))
    }
}

/// Parse all (msgid, msgstr) pairs from a `.mo` binary file.
///
/// The `.mo` format: magic, revision, N, offset_originals, offset_translations,
/// then N × (len, offset) tables for each side. Strings are null-terminated.
fn parse_mo_entries(data: &[u8]) -> Option<Vec<(String, String)>> {
    if data.len() < 28 {
        return None;
    }
    let r32 = |off: usize| -> Option<u32> {
        if off + 4 > data.len() {
            return None;
        }
        Some(u32::from_le_bytes(data[off..off + 4].try_into().ok()?))
    };

    let magic = r32(0)?;
    if magic != 0x950412de && magic != 0xde120495 {
        return None;
    }
    let n = r32(8)? as usize;
    let orig_off = r32(12)? as usize;
    let trans_off = r32(16)? as usize;

    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let o_len = r32(orig_off + i * 8)? as usize;
        let o_off = r32(orig_off + i * 8 + 4)? as usize;
        let t_len = r32(trans_off + i * 8)? as usize;
        let t_off = r32(trans_off + i * 8 + 4)? as usize;

        if o_off + o_len > data.len() || t_off + t_len > data.len() {
            continue;
        }
        let msgid: String = String::from_utf8_lossy(&data[o_off..o_off + o_len]).into_owned();
        let msgstr: String = String::from_utf8_lossy(&data[t_off..t_off + t_len]).into_owned();
        entries.push((msgid, msgstr));
    }
    Some(entries)
}

/// Build the final translations map from raw `.mo` entries, matching PHP's
/// `_translations_as_array()` logic:
/// - Singular: `msgid → msgstr`
/// - Plural (msgid contains `\0`): store both singular and plural forms
fn build_translations(entries: &[(String, String)]) -> Vec<(String, String)> {
    let mut rv = Vec::new();
    for (msgid, msgstr) in entries {
        if msgid.is_empty() {
            continue; // metadata entry
        }
        if let Some(null_pos) = msgid.find('\0') {
            // Plural form: "singular\0plural"
            let singular = &msgid[..null_pos];
            let plural = &msgid[null_pos + 1..];
            // msgstr format: "sing_trans\0plural_trans"
            if let Some(null_pos_t) = msgstr.find('\0') {
                rv.push((singular.to_string(), msgstr[..null_pos_t].to_string()));
                rv.push((plural.to_string(), msgstr[null_pos_t + 1..].to_string()));
            } else {
                rv.push((singular.to_string(), msgstr.clone()));
                rv.push((plural.to_string(), msgstr.clone()));
            }
        } else {
            rv.push((msgid.clone(), msgstr.clone()));
        }
    }
    rv
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
    if let Some(pref) = user_pref
        && !pref.is_empty() && pref != "auto"
            && available.iter().any(|a| a == pref) {
                return Some(pref.to_string());
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
///
/// Internal primitive — call sites use the bare [`__`], which reads the
/// per-request locale from the task-local (M10.2b).
pub fn translate(locale: Option<&str>, cache: &I18nCache, msg: &str) -> String {
    let Some(locale) = locale else {
        return msg.to_string();
    };
    let Some(catalog) = cache.catalog(locale) else {
        return msg.to_string();
    };
    catalog.gettext(msg).to_string()
}

/// Translate and format: `vsprintf(translate(fmt), args)`. Matches PHP's
/// `T_sprintf()`. Internal primitive (the bare [`t_sprintf`] reads the locale
/// from the task-local).
pub fn translate_sprintf(locale: Option<&str>, cache: &I18nCache, fmt: &str, args: &[&str]) -> String {
    let translated = translate(locale, cache, fmt);
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

/// Plural-aware translation: picks singular or plural form based on `n`,
/// matching PHP's `_ngettext()`.
pub fn ngettext(
    locale: Option<&str>,
    cache: &I18nCache,
    singular: &str,
    plural: &str,
    n: i64,
) -> String {
    let Some(locale) = locale else {
        return if n == 1 { singular.to_string() } else { plural.to_string() };
    };
    let Some(catalog) = cache.catalog(locale) else {
        return if n == 1 { singular.to_string() } else { plural.to_string() };
    };
    catalog.ngettext(singular, plural, n as u64).to_string()
}

/// Translate (with plural) and format: `vsprintf(_ngettext(...), args)`.
/// Matches PHP's `T_nsprintf()`. Internal primitive (the bare [`t_nsprintf`]
/// reads the locale from the task-local).
pub fn translate_nsprintf(
    locale: Option<&str>,
    cache: &I18nCache,
    singular: &str,
    plural: &str,
    n: i64,
    args: &[&str],
) -> String {
    let translated = ngettext(locale, cache, singular, plural, n);
    translate_sprintf(locale, cache, &translated, args)
}

/// The per-request translation context: the resolved locale + the shared
/// catalog. Stored in a task-local so call sites use the bare helpers below
/// without threading `(locale, cache)` (M10.2b).
pub struct I18nCtx {
    /// `None` = English (identity translation).
    pub locale: Option<String>,
    /// The shared catalog (a cheap `Arc` clone).
    pub cache: Arc<I18nCache>,
}

// The per-request `I18nCtx`. Set once at each entry point (post-auth) via
// `I18N.scope(...)`; the bare helpers read it. (A plain comment: the
// `task_local!` macro doesn't take a doc comment.)
tokio::task_local! {
    pub static I18N: I18nCtx;
}

/// Translate using the request's locale (from the task-local). Falls back to
/// the original (English) string when not scoped. The only plain-translate
/// call sites should use — the minijinja `__` is this same function.
pub fn __(msg: &str) -> String {
    I18N
        .try_with(|ctx| translate(ctx.locale.as_deref(), &ctx.cache, msg))
        .unwrap_or_else(|_| msg.to_string())
}

/// Translate + format (`T_sprintf`), using the request's locale. English
/// fallback: format without translating.
pub fn t_sprintf(fmt: &str, args: &[&str]) -> String {
    I18N
        .try_with(|ctx| translate_sprintf(ctx.locale.as_deref(), &ctx.cache, fmt, args))
        .unwrap_or_else(|_| crate::util::vsprintf(fmt, args))
}

/// Plural-aware translate + format (`T_nsprintf`), using the request's
/// locale. English fallback: pick the form by `n`, then format.
pub fn t_nsprintf(singular: &str, plural: &str, n: i64, args: &[&str]) -> String {
    I18N
        .try_with(|ctx| {
            translate_nsprintf(
                ctx.locale.as_deref(),
                &ctx.cache,
                singular,
                plural,
                n,
                args,
            )
        })
        .unwrap_or_else(|_| {
            let form = if n == 1 { singular } else { plural };
            crate::util::vsprintf(form, args)
        })
}

/// Resolve the locale for a request: reads the `USER_LANGUAGE` pref (if set
/// and not `"auto"`) and falls back to the `Accept-Language` header.
///
/// This is the per-request equivalent of PHP's `startup_gettext()`, which runs
/// at the top of every `index.php`/`api/` request. The resolved locale is
/// `None` for English (identity translation).
pub fn resolve_request_locale(
    cache: &I18nCache,
    accept_language: Option<&str>,
    user_language_pref: Option<&str>,
) -> Option<String> {
    resolve_locale(accept_language, user_language_pref, cache.available_locales())
}

/// Resolve the user's UI locale for a server-rendered surface (M10.3), the
/// way the shell and the RPC `init` do (M10.2): the profile-aware
/// `USER_LANGUAGE` preference, falling back to the request's `Accept-Language`
/// header, else English (`None`, identity translation). Reads the pref from the
/// per-process cache (one `SELECT` per (uid, profile), then memory hits).
pub async fn resolve_user_locale(
    state: &crate::AppState,
    uid: i64,
    profile: Option<i32>,
    accept_language: Option<&str>,
) -> Option<String> {
    let rows =
        crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let user_lang_pref = crate::prefs::resolve("USER_LANGUAGE", &rows, profile);
    let user_lang = user_lang_pref.as_str();
    resolve_request_locale(&state.i18n, accept_language, Some(user_lang))
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
    fn test_translations_map_de_de() {
        let cache = test_cache();
        let map = cache.translations_map("de_DE").expect("de_DE translations");
        assert!(map.len() > 100, "should have many entries, got {}", map.len());
        // Spot-check a known translation.
        let entry = map
            .iter()
            .find(|(k, _)| k == "Loading, please wait...")
            .expect("'Loading, please wait...' should be in the map");
        assert_ne!(entry.1, "Loading, please wait...", "should be translated");
    }

    #[test]
    fn test_translations_map_unknown_locale() {
        let cache = test_cache();
        assert!(cache.translations_map("xx_XX").is_none());
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
        let result = translate(None, &cache, "Hello");
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_translate_with_locale() {
        let cache = test_cache();
        let result = translate(Some("de_DE"), &cache, "Loading, please wait...");
        assert_ne!(result, "Loading, please wait...");
    }

    #[test]
    fn test_translate_missing_string_falls_back() {
        let cache = test_cache();
        let msg = "This string definitely does not exist in any catalog xyzzy 12345";
        let result = translate(Some("de_DE"), &cache, msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_t_sprintf() {
        let cache = test_cache();
        let result = translate_sprintf(None, &cache, "Hello %s, you have %d items", &["world", "5"]);
        assert_eq!(result, "Hello world, you have 5 items");
    }

    #[test]
    fn test_resolve_request_locale_pref_overrides_header() {
        let cache = test_cache();
        let locale = resolve_request_locale(&cache, Some("fr-FR"), Some("de_DE"));
        assert_eq!(locale.as_deref(), Some("de_DE"));
    }

    #[test]
    fn test_resolve_request_locale_header_only() {
        let cache = test_cache();
        let locale = resolve_request_locale(&cache, Some("de-DE,de;q=0.9"), None);
        assert_eq!(locale.as_deref(), Some("de_DE"));
    }

    #[test]
    fn test_resolve_request_locale_nothing() {
        let cache = test_cache();
        assert_eq!(resolve_request_locale(&cache, None, None), None);
    }

    #[test]
    fn test_resolve_request_locale_unavailable_pref_falls_through() {
        let cache = test_cache();
        // "xx_XX" is not a real locale; should fall through to Accept-Language.
        let locale = resolve_request_locale(&cache, Some("fr"), Some("xx_XX"));
        assert_eq!(locale.as_deref(), Some("fr_FR"));
    }

    #[test]
    fn test_ngettext_no_locale() {
        let cache = test_cache();
        assert_eq!(ngettext(None, &cache, "%d article", "%d articles", 1), "%d article");
        assert_eq!(ngettext(None, &cache, "%d article", "%d articles", 5), "%d articles");
    }

    #[test]
    fn test_ngettext_with_locale() {
        let cache = test_cache();
        // de_DE: "%d Artikel" (same for singular and plural in German)
        let result = ngettext(Some("de_DE"), &cache, "%d article", "%d articles", 1);
        assert_ne!(result, "1 article", "should be translated");
    }

    #[test]
    fn test_t_nsprintf() {
        let cache = test_cache();
        let result = translate_nsprintf(None, &cache, "%d article", "%d articles", 5, &["5"]);
        assert_eq!(result, "5 articles");
        let result = translate_nsprintf(None, &cache, "%d article", "%d articles", 1, &["1"]);
        assert_eq!(result, "1 article");
    }
}
