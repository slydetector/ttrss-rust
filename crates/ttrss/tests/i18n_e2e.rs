//! M10.6 — i18n end-to-end verification across 5 locales.
//!
//! Verifies the request path (locale resolution → task-local → `__()`)
//! actually resolves each request's locale and looks up the real gettext
//! catalogs (`tt-rss/locale/<lang>/LC_MESSAGES/messages.mo`). For 5 locales, a
//! shell string, the API's "Feed not found." error, and a plugin toolbar
//! string are each verified against the catalog.
//!
//! Edge cases: a locale with no catalog falls back to the English original; a
//! string missing from a catalog falls back to the original while other strings
//! in the same catalog are translated; plural forms pick the right form by
//! count. The `get_translations()` dropdown keys match the PHP's.
//!
//! The expected values are computed from the same `.mo` files the port
//! consumes, so the test verifies the request-path wiring (locale resolution +
//! task-local + catalog lookup), not the catalog contents (the community's).
//!
//! Rust-only: the frozen PHP suite has no multi-locale e2e.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use test_db::template::with_test_db;
use tower::ServiceExt;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::i18n::{I18nCache, ngettext, translate};
use ttrss::pref_ui::get_translations;

fn test_config(plugins: &str) -> Config {
    let mut c = common::test_config();
    c.plugins = plugins.into();
    c.docroot = tt_rss_dir();
    c
}

/// The 5 locales under test — each has all three sample strings in its
/// catalog (verified against `tt-rss/locale/*/LC_MESSAGES/messages.mo`).
const LOCALES: [&str; 5] = ["de_DE", "fr_FR", "ru_RU", "it_IT", "es"];
const SHELL_STR: &str = "Loading, please wait...";
const API_STR: &str = "Feed not found.";
const PLUGIN_STR: &str = "Toggle sidebar";

fn tt_rss_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss")
}

/// A catalog loaded straight from the frozen `.mo` files (the source of truth
/// for the expected values).
fn catalog() -> I18nCache {
    I18nCache::new(tt_rss_dir().join("locale"))
}

/// The expected translation of `msg` in `locale` (from the `.mo` catalog).
fn expected(locale: &str, msg: &str) -> String {
    catalog()
        .catalog(locale)
        .expect("locale must have a catalog")
        .gettext(msg)
        .to_string()
}

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

/// Persist a fully-logged-in admin session (uid 1 + CSRF) so both the shell
/// and the CSRF-gated RPCs pass.
async fn save_admin_session(state: &AppState) {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    ttrss::session::save(&state.db, None, "e2e-sid", &data)
        .await
        .expect("session save");
}

/// Point uid 1's `USER_LANGUAGE` at `locale` (a fresh value each time).
async fn set_user_language(state: &AppState, locale: &str) {
    sqlx::query(
        "DELETE FROM ttrss_user_prefs2 WHERE owner_uid = 1 AND pref_name = 'USER_LANGUAGE'",
    )
    .execute(state.db.pool())
    .await
    .expect("clear USER_LANGUAGE");
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'USER_LANGUAGE', NULL, $1)",
    )
    .bind(locale)
    .execute(state.db.pool())
    .await
    .expect("set USER_LANGUAGE");
}

/// GET the authenticated shell. Returns the HTML body.
async fn shell_page(state: &AppState) -> String {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss/index.php")
        .header("cookie", "ttrss_sid=e2e-sid")
        .body(Body::empty())
        .expect("request");
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "shell must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

/// GET the `view` RPC for a nonexistent feed. Returns the body (the
/// "Feed not found." error lands in the content).
async fn view_not_found(state: &AppState) -> String {
    let req = Request::builder()
        .method(Method::GET)
        .uri(
            "/tt-rss/backend.php?op=Feeds&method=view&csrf_token=tok\
             &clientLocation=http%3A%2F%2F127.0.0.1%3A9000%2Ftt-rss%2Findex.php&feed=999999",
        )
        .header("cookie", "ttrss_sid=e2e-sid")
        .body(Body::empty())
        .expect("request");
    let resp = ttrss::http::build_router(state)
        .with_state(state.clone())
        .oneshot(req)
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "view must be 200");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The shell renders the request's locale (a shell string, 5 locales).
///
/// A **fresh `AppState` per locale**: the `USER_LANGUAGE` pref is read through
/// the per-`AppState` `prefs_cache` (`prefs::load_rows_cached`), so a locale
/// change in the DB only takes effect on a fresh cache.
#[tokio::test]
async fn test_cross_locale_shell() {
    with_test_db(|url| {
        Box::pin(async move {
            for locale in LOCALES {
                let state = setup(&url, "auth_internal").await;
                save_admin_session(&state).await;
                set_user_language(&state, locale).await;
                let want = expected(locale, SHELL_STR);
                let html = shell_page(&state).await;
                assert!(
                    html.contains("<!-- order 0, default -->"),
                    "must be the shell, not the login page"
                );
                assert!(
                    html.contains(&want),
                    "[{locale}] shell must carry the translated {SHELL_STR:?} = {want:?}"
                );
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// The API `view` "not found" error carries the request's locale (5 locales).
#[tokio::test]
async fn test_cross_locale_api_view() {
    with_test_db(|url| {
        Box::pin(async move {
            for locale in LOCALES {
                let state = setup(&url, "auth_internal").await;
                save_admin_session(&state).await;
                set_user_language(&state, locale).await;
                let want = expected(locale, API_STR);
                let body = view_not_found(&state).await;
                assert!(
                    body.contains(&want),
                    "[{locale}] view must carry the translated {API_STR:?} = {want:?}"
                );
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A plugin toolbar string carries the request's locale (5 locales) — the
/// `toggle_sidebar` button is enabled and rendered in the shell.
#[tokio::test]
async fn test_cross_locale_plugin() {
    with_test_db(|url| {
        Box::pin(async move {
            for locale in LOCALES {
                let state = setup(&url, "auth_internal,toggle_sidebar").await;
                save_admin_session(&state).await;
                set_user_language(&state, locale).await;
                let want = expected(locale, PLUGIN_STR);
                let html = shell_page(&state).await;
                assert!(
                    html.contains(&want),
                    "[{locale}] shell must carry the plugin's translated {PLUGIN_STR:?} = {want:?}"
                );
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A locale with no catalog falls back to the English original (e2e shell).
/// "zz_ZZ" is not an available locale, so the resolution must not pick it and
/// the shell stays English.
#[tokio::test]
async fn test_unavailable_locale_falls_back_english() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "auth_internal").await;
            save_admin_session(&state).await;
            set_user_language(&state, "zz_ZZ").await;
            let html = shell_page(&state).await;
            assert!(
                html.contains(SHELL_STR),
                "unavailable locale must fall back to the English original"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A string missing from a catalog falls back to the English original, while
/// other strings in the same catalog are translated. The `eo` (Esperanto)
/// catalog lacks "Loading, please wait..." but has "Save".
#[test]
fn test_missing_string_falls_back_original() {
    let cat = catalog();
    let missing = "Loading, please wait..."; // absent from the eo catalog
    assert_eq!(
        translate(Some("eo"), &cat, missing),
        missing,
        "a string missing from the catalog must return the original"
    );
    let present = "Save"; // present in the eo catalog
    let eo_save = expected("eo", present);
    assert_ne!(
        eo_save, present,
        "the eo catalog must actually translate 'Save'"
    );
    assert_eq!(translate(Some("eo"), &cat, present), eo_save);
}

/// Plural forms: the count picks the right form. `(%d day)`/`(%d days)`.
///
/// German is two-form (1 → singular, N → plural), handled by the `gettext`
/// crate. Russian is three-form (one/few/other); its gettext rule is the
/// nested-ternary form the `gettext` 0.4.0 crate evaluates incorrectly, so
/// ttrss picks the form via CLDR (ICU4X) instead — see
/// `i18n::I18nCache::cldr_form_index`. Both are asserted here.
#[test]
fn test_plural_forms() {
    let cat = catalog();
    // German: 1 → "(%d Tag)", 5 → "(%d Tage)".
    assert_eq!(
        ngettext(Some("de_DE"), &cat, "(%d day)", "(%d days)", 1),
        "(%d Tag)"
    );
    assert_eq!(
        ngettext(Some("de_DE"), &cat, "(%d day)", "(%d days)", 5),
        "(%d Tage)"
    );
    // Russian (three-form, CLDR-picked): one/few/other = день/дня/дней.
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 1),
        "(%d день)"
    );
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 2),
        "(%d дня)"
    );
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 5),
        "(%d дней)"
    );
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 21),
        "(%d день)"
    );
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 22),
        "(%d дня)"
    );
    assert_eq!(
        ngettext(Some("ru_RU"), &cat, "(%d day)", "(%d days)", 25),
        "(%d дней)"
    );
    // No locale → the plain English forms by count.
    assert_eq!(ngettext(None, &cat, "(%d day)", "(%d days)", 1), "(%d day)");
    assert_eq!(
        ngettext(None, &cat, "(%d day)", "(%d days)", 5),
        "(%d days)"
    );
}

/// The `get_translations()` dropdown keys match the PHP's (the
/// `USER_LANGUAGE` select options). Order included (declaration order).
#[test]
fn test_get_translations_matches_php() {
    let keys: Vec<&str> = get_translations().iter().map(|(k, _)| *k).collect();
    // The PHP's keys (include/functions.php `get_translations()`), in order.
    let php_keys = [
        "auto", "ar_SA", "be", "bg_BG", "da_DA", "ca_CA", "cs_CZ", "en_US", "el_GR", "es", "de_DE",
        "fa", "fr_FR", "gl", "hu_HU", "it_IT", "ja_JP", "lv_LV", "nb_NO", "nl_NL", "pl_PL",
        "ru_RU", "pt_BR", "pt_PT", "zh_CN", "zh_TW", "uk_UA", "sv_SE", "fi_FI", "ta", "tr_TR",
    ];
    assert_eq!(
        keys,
        php_keys.to_vec(),
        "the get_translations() keys must match the PHP's, in order"
    );
}
