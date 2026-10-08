//! `update.php` user verbs (docs/19 P0) — the `UserHelper` primitives ported
//! to `ttrss::users` for the in-process first-boot bootstrap.
//!
//! There is no PHP CLI test for these (checked `tests_functional/`: `update.php`
//! has no dedicated suite), so the port gets its own coverage — the same bar
//! as the `Db_Migrations` port (docs/20).

mod common;

use test_db::template::with_test_db;
use ttrss::core::db::Db;
use ttrss::core::user;

async fn make_db(url: &str) -> Db {
    common::make_db_from_url(url).await.expect("test db")
}

/// Seed a user the way `UserHelperTest::setUp` does (known SSHA-512 hash).
async fn seed(db: &Db, login: &str, password: &str, level: i32) -> i32 {
    let (hash, salt) = test_db::ssha512_password(login, password);
    test_db::seed_user(db.pool(), login, &hash, &salt, level)
        .await
        .expect("seed user")
}

// ── user_add ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_user_add_creates_user() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            assert!(
                ttrss::users::user_add(db.pool(), "VerbUser", "secret123", 0)
                    .await
                    .unwrap()
            );

            // Login stored lowercase (PHP `mb_strtolower`), SSHA-512 hash, level set.
            let row: (i32, String, String, i32) = sqlx::query_as(
                "SELECT id, login, pwd_hash, access_level FROM ttrss_users WHERE login = $1",
            )
            .bind("verbuser")
            .fetch_one(db.pool())
            .await
            .expect("row");
            assert!(
                row.2.starts_with("SSHA-512:"),
                "new passwords hash SSHA-512"
            );
            assert_eq!(row.3, 0);
            // The password verifies against the stored salt.
            let (pwd_hash, salt): (String, String) =
                sqlx::query_as("SELECT pwd_hash, salt FROM ttrss_users WHERE id = $1")
                    .bind(row.0)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert!(user::verify_password(&pwd_hash, "secret123", &salt));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_add_duplicate_rejected() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            seed(&db, "verbuser", "a", 0).await;
            assert!(
                !ttrss::users::user_add(db.pool(), "VERBUSER", "b", 0)
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_add_invalid_level_rejected() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            // 7 is not a real access level (`map_access_level` → KEEP_CURRENT).
            assert!(
                !ttrss::users::user_add(db.pool(), "verbuser", "a", 7)
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_add_blank_inputs_rejected() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            assert!(!ttrss::users::user_add(db.pool(), "", "a", 0).await.unwrap());
            assert!(
                !ttrss::users::user_add(db.pool(), "<br>", "a", 0)
                    .await
                    .unwrap()
            );
            assert!(
                !ttrss::users::user_add(db.pool(), "verbuser", "", 0)
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── user_modify ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_user_modify_password_and_level() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let uid = seed(&db, "verbuser", "oldpass", 0).await;
            assert!(
                ttrss::users::user_modify(db.pool(), uid, Some("newpass"), 5)
                    .await
                    .unwrap()
            );

            let (pwd_hash, salt, level): (String, String, i32) = sqlx::query_as(
                "SELECT pwd_hash, salt, access_level FROM ttrss_users WHERE id = $1",
            )
            .bind(uid)
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert!(!user::verify_password(&pwd_hash, "oldpass", &salt));
            assert!(user::verify_password(&pwd_hash, "newpass", &salt));
            assert!(pwd_hash.starts_with("SSHA-512:"));
            assert_eq!(level, 5);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_modify_unknown_user() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            assert!(
                !ttrss::users::user_modify(db.pool(), 999_999, Some("x"), 0)
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── user_has_password / user_check_password ─────────────────────────────────

#[tokio::test]
async fn test_user_check_password() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let uid = seed(&db, "verbuser", "the-password", 0).await;
            assert!(
                ttrss::users::user_has_password(db.pool(), uid, "the-password")
                    .await
                    .unwrap()
            );
            assert!(
                !ttrss::users::user_has_password(db.pool(), uid, "wrong")
                    .await
                    .unwrap()
            );
            assert!(
                ttrss::users::user_check_password(db.pool(), "verbuser", "the-password")
                    .await
                    .unwrap()
            );
            assert!(
                !ttrss::users::user_check_password(db.pool(), "verbuser", "nope")
                    .await
                    .unwrap()
            );
            // Unknown user → false (PHP: `find_user_by_login` null → false).
            assert!(
                !ttrss::users::user_check_password(db.pool(), "ghost", "x")
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── user_exists / user_set_password / user_set_access_level ─────────────────

#[tokio::test]
async fn test_user_exists_case_insensitive() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            seed(&db, "verbuser", "a", 0).await;
            assert!(
                ttrss::users::user_exists(db.pool(), "VERBUSER")
                    .await
                    .unwrap()
            );
            assert!(!ttrss::users::user_exists(db.pool(), "ghost").await.unwrap());
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_set_password() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            seed(&db, "verbuser", "oldpass", 0).await;
            assert!(
                ttrss::users::user_set_password(db.pool(), "verbuser", "brand-new")
                    .await
                    .unwrap()
            );
            assert!(
                ttrss::users::user_check_password(db.pool(), "verbuser", "brand-new")
                    .await
                    .unwrap()
            );
            assert!(
                !ttrss::users::user_set_password(db.pool(), "ghost", "x")
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_set_access_level() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let uid = seed(&db, "verbuser", "a", 0).await;
            assert!(
                ttrss::users::user_set_access_level(db.pool(), "verbuser", 10)
                    .await
                    .unwrap()
            );
            let level: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE id = $1")
                    .bind(uid)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level, 10);
            // Invalid level → false, unchanged.
            assert!(
                !ttrss::users::user_set_access_level(db.pool(), "verbuser", 3)
                    .await
                    .unwrap()
            );
            let level: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE id = $1")
                    .bind(uid)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level, 10);
            assert!(
                !ttrss::users::user_set_access_level(db.pool(), "ghost", 0)
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── user_enable_api ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_user_enable_api_round_trip() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let uid = seed(&db, "verbuser", "a", 0).await;

            assert!(ttrss::users::user_enable_api(&db, "verbuser", true).await);
            let on: Option<String> = sqlx::query_scalar(
                "SELECT value FROM ttrss_user_prefs2
             WHERE pref_name = $1 AND owner_uid = $2 AND profile IS NULL",
            )
            .bind(ttrss::prefs::ENABLE_API_ACCESS)
            .bind(uid as i64)
            .fetch_optional(db.pool())
            .await
            .unwrap();
            assert_eq!(on.as_deref(), Some("1")); // bool true stores "1"

            assert!(ttrss::users::user_enable_api(&db, "verbuser", false).await);
            let off: Option<String> = sqlx::query_scalar(
                "SELECT value FROM ttrss_user_prefs2
             WHERE pref_name = $1 AND owner_uid = $2 AND profile IS NULL",
            )
            .bind(ttrss::prefs::ENABLE_API_ACCESS)
            .bind(uid as i64)
            .fetch_optional(db.pool())
            .await
            .unwrap();
            assert_eq!(off.as_deref(), Some("")); // bool false stores ""

            assert!(!ttrss::users::user_enable_api(&db, "ghost", true).await);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── user_remove ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_user_remove() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            seed(&db, "verbuser", "a", 0).await;
            assert!(
                ttrss::users::user_remove(db.pool(), "verbuser")
                    .await
                    .unwrap()
            );
            assert!(
                !ttrss::users::user_exists(db.pool(), "verbuser")
                    .await
                    .unwrap()
            );
            // Already gone.
            assert!(
                !ttrss::users::user_remove(db.pool(), "verbuser")
                    .await
                    .unwrap()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
