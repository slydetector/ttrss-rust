//! DB tests for the `Pref_Prefs` app-password handlers (M4): `generateAppPassword`
//! and `deleteAppPasswords` (plus the shared `appPasswordList` renderer).
//!
//! Rust-only: the frozen PHP suite has no `Pref_Prefs` configuration tests, so
//! these exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures. Owner 1 (the admin user in `schema.sql`) is the session uid.

mod common;

use serde_json::{Map, Value, json};
use sqlx::Row;
use test_db::template::with_test_db;

type Pool = sqlx::PgPool;

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// The plaintext password from a `generateAppPassword` response (the text
/// between `<strong>` and `</strong>` in the warning div).
fn extract_strong(html: &str) -> String {
    let start = html.find("<strong>").expect("warning div") + "<strong>".len();
    let end = html.find("</strong>").expect("warning close");
    html[start..end].to_string()
}

/// The stored app-password rows for uid 1 (id, title, service, pwd_hash, last_used).
async fn fetch(db: &Pool) -> Vec<(i32, String, String, String, Option<String>)> {
    let rows = sqlx::query(
        "SELECT id, title, service, pwd_hash, last_used::text AS last_used \
         FROM ttrss_app_passwords WHERE owner_uid = 1 ORDER BY title",
    )
    .fetch_all(db)
    .await
    .unwrap();
    rows.iter()
        .map(|r| {
            (
                r.get::<i32, _>(0),
                r.get::<String, _>(1),
                r.get::<String, _>(2),
                r.get::<String, _>(3),
                r.get::<Option<String>, _>(4),
            )
        })
        .collect()
}

#[tokio::test]
async fn test_generate_app_password() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            let resp = ttrss::settings::generate_app_password(
                &state,
                &sess,
                &request(&[("title", json!("My API"))]),
            )
            .await
            .unwrap();

            let rows = fetch(db).await;
            assert_eq!(rows.len(), 1);
            let (_id, title, service, pwd_hash, last_used) = &rows[0];
            assert_eq!(title, "My API");
            assert_eq!(service, "_api");
            assert!(pwd_hash.starts_with("SSHA-512:"), "got: {pwd_hash}");
            // A freshly generated password has never been used.
            assert!(last_used.is_none());

            // The plaintext is shown in the warning and verifies against the stored hash.
            let pw = extract_strong(&resp);
            assert_eq!(pw.len(), 16, "got: {pw}");
            // All 16 chars are distinct.
            assert_eq!(
                pw.chars()
                    .collect::<std::collections::HashSet<char>>()
                    .len(),
                16
            );
            assert!(ttrss::core::user::verify_app_password(pwd_hash, &pw));

            // The response is the warning div + the refreshed list.
            assert!(resp.contains("Generated password"));
            assert!(resp.contains("Please remember it for future reference."));
            assert!(resp.contains("app-password-list"));
            assert!(resp.contains("My API"));
            // `last_used` is NULL → "Never".
            assert!(resp.contains("Never"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// API login with an out-of-date app-password algorithm: the successful
/// match re-hashes to SSHA-512 (same salt) — `check_app_password`'s on-match
/// upgrade (the PHP's "Upgrading app password of user … to SSHA-512").
#[tokio::test]
async fn test_api_login_upgrades_outdated_app_password_algo() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            // A legacy SHA1X-format app password (pre-SSHA-512 installations).
            let pw = "old-style-pass";
            let salt = "somesalt123";
            let stored = format!(
                "{}:{salt}",
                ttrss::core::user::hash_password(pw, salt, ttrss::core::user::ALGO_SHA1X).unwrap()
            );
            assert!(stored.starts_with("SHA1X:"));
            sqlx::query(
                "INSERT INTO ttrss_app_passwords (title, owner_uid, pwd_hash, service, created) \
                 VALUES ('legacy', 1, $1, '_api', NOW())",
            )
            .bind(&stored)
            .execute(db)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
                 VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(db)
            .await
            .unwrap();

            let body = request(&[("user", json!("admin")), ("password", json!(pw))]);
            let (status, content) =
                ttrss::api::before_and_dispatch(&state, &sess, "login", &body, None).await;
            assert_eq!(status, 0, "login must succeed: {content}");

            // The stored row is now SSHA-512, salt preserved, still verifies.
            let new_stored: String = sqlx::query_scalar(
                "SELECT pwd_hash FROM ttrss_app_passwords WHERE title = 'legacy'",
            )
            .fetch_one(db)
            .await
            .unwrap();
            assert!(new_stored.starts_with("SSHA-512:"), "got: {new_stored}");
            assert!(
                new_stored.ends_with(salt),
                "salt must be preserved: {new_stored}"
            );
            assert!(ttrss::core::user::verify_app_password(&new_stored, pw));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_generate_app_password_unique_chars() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            // The alphabet (no vowels / `l`/`o`/`u` lookalikes).
            const ALPHABET: &[u8] = b"0123456789abcdfghjkmnpqrstvwxyzABCDFGHJKMNPQRSTVWXYZ*%+^";

            for _ in 0..20 {
                let resp = ttrss::settings::generate_app_password(
                    &state,
                    &sess,
                    &request(&[("title", json!("x"))]),
                )
                .await
                .unwrap();
                let pw = extract_strong(&resp);
                assert_eq!(pw.len(), 16);
                for c in pw.bytes() {
                    assert!(ALPHABET.contains(&c), "char {c:?} not in alphabet");
                }
            }
            assert_eq!(fetch(db).await.len(), 20);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_delete_app_passwords() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            // Two passwords.
            for t in ["A", "B"] {
                let _ = ttrss::settings::generate_app_password(
                    &state,
                    &sess,
                    &request(&[("title", json!(t))]),
                )
                .await
                .unwrap();
            }
            let rows = fetch(db).await;
            assert_eq!(rows.len(), 2);
            let keep_id = rows[0].0;
            let del_id = rows[1].0;

            // Delete one (the "B" row, the later title).
            let resp = ttrss::settings::delete_app_passwords(
                &state,
                &sess,
                &request(&[("ids", json!([del_id]))]),
            )
            .await
            .unwrap();

            let rows = fetch(db).await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, keep_id);
            // The refreshed list shows only the surviving row (one data row, no `>B<`).
            assert!(resp.contains("app-password-list"));
            assert_eq!(resp.matches("<tr data-row-id=").count(), 1);
            assert!(
                !resp.contains(">B<"),
                "deleted title should not be in the list"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_delete_app_passwords_foreign_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            // A password owned by a *different* user must not be deleted.
            ttrss::settings::generate_app_password(
                &state,
                &sess,
                &request(&[("title", json!("mine"))]),
            )
            .await
            .unwrap();
            let my_id = fetch(db).await[0].0;
            // Reassign it to a (nonexistent-for-uid-1) owner... use a fresh user.
            sqlx::query(
                "INSERT INTO ttrss_users (id, login, pwd_hash) VALUES (2, 'u2', 'SHA1:00')",
            )
            .execute(db)
            .await
            .unwrap();
            sqlx::query("UPDATE ttrss_app_passwords SET owner_uid = 2 WHERE id = $1")
                .bind(my_id)
                .execute(db)
                .await
                .unwrap();

            // Now uid 1 owns nothing; deleting `my_id` as uid 1 is a no-op.
            let _ = ttrss::settings::delete_app_passwords(
                &state,
                &sess,
                &request(&[("ids", json!([my_id]))]),
            )
            .await
            .unwrap();
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ttrss_app_passwords WHERE id = $1")
                    .bind(my_id)
                    .fetch_one(db)
                    .await
                    .unwrap();
            assert_eq!(count, 1, "foreign row must survive");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_delete_app_passwords_empty_ids_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            ttrss::settings::generate_app_password(
                &state,
                &sess,
                &request(&[("title", json!("keep"))]),
            )
            .await
            .unwrap();

            // No `ids` → no delete (avoids `IN ()`); the row survives, list still renders.
            let resp = ttrss::settings::delete_app_passwords(&state, &sess, &request(&[]))
                .await
                .unwrap();
            assert_eq!(fetch(db).await.len(), 1);
            assert!(resp.contains("app-password-list"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_app_password_list_escapes_title() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = common::make_state(&url).await;
            let db = state.db.pool();

            // A title with HTML-special characters (stored raw, cleaned only).
            let title = "a&b<c";
            sqlx::query(
                "INSERT INTO ttrss_app_passwords (title, owner_uid, pwd_hash, service, created) \
             VALUES ($1, 1, 'SSHA-512:x:y', '_api', now())",
            )
            .bind(title)
            .execute(db)
            .await
            .unwrap();

            let html = ttrss::settings::app_password_list(&state, &sess)
                .await
                .unwrap();
            // The title is html-escaped in the rendered table.
            assert!(html.contains("a&amp;b&lt;c"), "got:\n{html}");
            assert!(!html.contains("a&b<c"), "raw title must not appear");
            Ok(())
        })
    })
    .await
    .unwrap();
}
