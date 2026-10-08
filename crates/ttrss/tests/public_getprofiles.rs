//! Parity tests for `Handler_Public::getProfiles` (the `op=getprofiles`
//! public.php endpoint the login page's Profile dropdown fetches) — a faithful
//! port of the PHP method: `[{label, value}, …]` with the `Default profile`
//! entry first, profiles title-asc, case-insensitive login match.
//!
//! Drives `ttrss::auth::get_profiles` directly against a fresh Postgres
//! fixture. The axum layer above it (`ttrss::http`) is covered at the HTTP
//! level in `public_http.rs` (params from the form body, as the login page
//! sends them).

mod common;

use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::auth;

async fn make_state(url: &str) -> AppState {
    let db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(common::test_config(), db)
}

/// Insert a user + profiles; returns the user id.
async fn add_user_with_profiles(state: &AppState, login: &str, profiles: &[&str]) -> i32 {
    let pool = state.db.pool();
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level) VALUES ($1, 'SHA1:x', 0) RETURNING id",
    )
    .bind(login)
    .fetch_one(pool)
    .await
    .expect("user insert");
    for title in profiles {
        sqlx::query("INSERT INTO ttrss_settings_profiles (owner_uid, title) VALUES ($1, $2)")
            .bind(uid)
            .bind(title)
            .execute(pool)
            .await
            .expect("profile insert");
    }
    uid
}

#[tokio::test]
async fn test_getprofiles_empty_login_returns_empty() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let v = auth::get_profiles(&state.db, "").await;
            assert_eq!(v, serde_json::json!([]), "no login -> []: {v}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_getprofiles_unknown_login_returns_default_only() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let v = auth::get_profiles(&state.db, "nouser").await;
            assert_eq!(
                v,
                serde_json::json!([ { "label": "Default profile", "value": 0 } ]),
                "unknown login -> default only: {v}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_getprofiles_lists_profiles_title_asc() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            // Inserted out of order; the response must be title-ascending.
            let uid = add_user_with_profiles(&state, "profuser", &["Zeta", "Alpha", "Mid"]).await;

            let v = auth::get_profiles(&state.db, "profuser").await;
            let arr = v.as_array().expect("array");
            assert_eq!(arr.len(), 4, "default + 3 profiles: {v}");
            assert_eq!(
                arr[0],
                serde_json::json!({ "label": "Default profile", "value": 0 })
            );
            let titles: Vec<&str> = arr[1..]
                .iter()
                .map(|p| p["label"].as_str().unwrap())
                .collect();
            assert_eq!(titles, vec!["Alpha", "Mid", "Zeta"], "title asc: {v}");
            // `value` carries the profile id (INT4).
            let alpha_id: i32 = sqlx::query_scalar(
                "SELECT id FROM ttrss_settings_profiles WHERE owner_uid = $1 AND title = 'Alpha'",
            )
            .bind(uid)
            .fetch_one(state.db.pool())
            .await
            .expect("alpha id");
            assert_eq!(arr[1]["value"], serde_json::json!(alpha_id));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_getprofiles_login_case_insensitive() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            add_user_with_profiles(&state, "CaseUser", &["P1"]).await;

            for login in ["caseuser", "CASEUSER", "CaseUser"] {
                let v = auth::get_profiles(&state.db, login).await;
                let arr = v.as_array().expect("array");
                assert_eq!(arr.len(), 2, "case-insensitive match for {login:?}: {v}");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}
