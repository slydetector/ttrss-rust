//! DB test for the shell's locale resolution (M10.2) — the `USER_LANGUAGE`
//! pref read in the `index` handler. Regression guard for the wrong-column
//! names bug: the query (`val_string`/`name` vs the real `value`/`pref_name`)
//! errored on every request, the error was swallowed, and the shell always
//! fell back to `Accept-Language`, ignoring the user's Language setting.
//!
//! Rust-only: the frozen PHP suite has no shell-rendering locale test.

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

fn test_config() -> Config {
    let mut c = common::test_config();
    // The real docroot so the gettext catalogs load (docs: i18n parity).
    c.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    c
}

async fn setup(url: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(), db)
}

/// Persist a logged-in session (uid 1) and GET `/tt-rss/index.php` with its
/// cookie (the browser's authenticated shell request).
async fn shell_page(state: &AppState, accept_language: Option<&str>) -> String {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    ttrss::session::save(&state.db, None, "test-sid", &data)
        .await
        .expect("session save");
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri("/tt-rss/index.php")
        .header("cookie", "ttrss_sid=test-sid");
    if let Some(lang) = accept_language {
        builder = builder.header("accept-language", lang);
    }
    let req = builder.body(Body::empty()).expect("request");
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

/// The user's `USER_LANGUAGE` pref wins over a (missing) `Accept-Language`
/// header: the shell renders the translated strings.
#[tokio::test]
async fn test_shell_uses_user_language_pref() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url).await;
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'USER_LANGUAGE', NULL, 'de_DE')",
            )
            .execute(state.db.pool())
            .await
            .expect("set USER_LANGUAGE");

            let html = shell_page(&state, None).await;
            assert!(
                html.contains("<!-- order 0, default -->"),
                "must be the shell, not the login page"
            );
            assert!(
                html.contains("Ladevorgang, bitte warten..."),
                "German pref must translate the shell strings"
            );
            assert!(
                !html.contains("Loading, please wait..."),
                "English string must be gone when the German pref is set"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// Without the pref, the shell stays English (the `Accept-Language`
/// fallback with no header → default locale).
#[tokio::test]
async fn test_shell_stays_english_without_pref() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url).await;

            let html = shell_page(&state, None).await;
            assert!(
                html.contains("Loading, please wait..."),
                "English shell expected"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
