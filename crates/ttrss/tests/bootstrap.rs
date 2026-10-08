//! The in-process first-boot bootstrap (`ttrss::bootstrap`) — the reference
//! `startup.sh`'s user-management steps (docs/19 P0). No PHP suite covers
//! these (they live in the docker startup script, not `update.php`'s test
//! surface), so the port carries its own coverage — the same bar as the
//! `users` verbs and `Db_Migrations` ports.
//!
//! The shared test template seeds the stock `admin`/`password` user
//! (`schema.sql`), matching the reference's fresh-install state.

mod common;

use test_db::template::with_test_db;
use ttrss::bootstrap::{self, BootstrapVars};
use ttrss::core::db::Db;
use ttrss::users;

async fn make_db(url: &str) -> Db {
    common::make_db_from_url(url).await.expect("test db")
}

/// The whole user table + pref rows — the "nothing changed" snapshot.
async fn snapshot(db: &Db) -> (Vec<(String, String, String, i32)>, Vec<(String, i64)>) {
    let users: Vec<(String, String, String, i32)> = sqlx::query_as(
        "SELECT login, pwd_hash, salt, access_level
         FROM ttrss_users ORDER BY id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let prefs: Vec<(String, i64)> =
        sqlx::query_as("SELECT pref_name, owner_uid FROM ttrss_user_prefs2 ORDER BY 1, 2")
            .fetch_all(db.pool())
            .await
            .unwrap();
    (users, prefs)
}

// ── admin password policy ───────────────────────────────────────────────────

#[tokio::test]
async fn test_admin_user_pass_applied() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            assert!(
                users::user_check_password(db.pool(), "admin", "password")
                    .await
                    .unwrap()
            );

            let vars = BootstrapVars {
                admin_user_pass: Some("new-admin-pass".into()),
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert!(report.admin_password_set);
            assert!(report.admin_password_randomized.is_none());
            assert!(
                users::user_check_password(db.pool(), "admin", "new-admin-pass")
                    .await
                    .unwrap()
            );
            assert!(
                !users::user_check_password(db.pool(), "admin", "password")
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
async fn test_admin_password_randomized_when_stock() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");

            let report = bootstrap::run(&db, &BootstrapVars::default(), data.path())
                .await
                .unwrap();
            let Some(new_pass) = report.admin_password_randomized else {
                panic!("stock password must be randomized on a fresh DB");
            };
            // The reference's `tr -dc A-Za-z0-9 | head -c 16` shape.
            assert_eq!(new_pass.len(), 16);
            assert!(new_pass.chars().all(|c| c.is_ascii_alphanumeric()));
            assert!(
                users::user_check_password(db.pool(), "admin", &new_pass)
                    .await
                    .unwrap()
            );
            assert!(
                !users::user_check_password(db.pool(), "admin", "password")
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
async fn test_admin_password_untouched_when_not_stock() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            users::user_set_password(db.pool(), "admin", "custom-pass")
                .await
                .unwrap();
            let hash_before: String =
                sqlx::query_scalar("SELECT pwd_hash FROM ttrss_users WHERE login = $1")
                    .bind("admin")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();

            // No vars: the admin password is not the stock one → untouched.
            let report = bootstrap::run(&db, &BootstrapVars::default(), data.path())
                .await
                .unwrap();
            assert!(report.admin_password_randomized.is_none());
            assert!(
                users::user_check_password(db.pool(), "admin", "custom-pass")
                    .await
                    .unwrap()
            );
            let hash_after: String =
                sqlx::query_scalar("SELECT pwd_hash FROM ttrss_users WHERE login = $1")
                    .bind("admin")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(hash_before, hash_after);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── admin access level ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_admin_level_applied() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            let vars = BootstrapVars {
                admin_user_pass: Some("x".into()), // suppress the randomize step
                admin_user_access_level: Some("10".into()),
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert!(report.admin_level_set);
            let level: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE login = $1")
                    .bind("admin")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level, 10);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_admin_level_invalid_fails() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            let level_before: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE login = $1")
                    .bind("admin")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            let vars = BootstrapVars {
                admin_user_pass: Some("x".into()),
                admin_user_access_level: Some("7".into()),
                ..Default::default()
            };
            let err = bootstrap::run(&db, &vars, data.path())
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("Invalid access level value: 7"), "{err}");
            // Fail-closed: the server exits (main's `?`) — the level is
            // unchanged.
            let level_after: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE login = $1")
                    .bind("admin")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level_before, level_after);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── auto-create user ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_auto_user_created() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            let vars = BootstrapVars {
                // Suppress the admin randomize (deterministic snapshot below).
                admin_user_pass: Some("x".into()),
                auto_create_user: Some("autouser".into()),
                auto_create_user_pass: "auto-pass".into(),
                auto_create_user_access_level: String::new(), // → 0, as PHP
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert!(report.auto_user_created);
            let level: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE login = $1")
                    .bind("autouser")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level, 0);
            assert!(
                users::user_check_password(db.pool(), "autouser", "auto-pass")
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
async fn test_auto_user_existing_untouched() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            users::user_add(db.pool(), "autouser", "original-pass", 5)
                .await
                .unwrap();

            let vars = BootstrapVars {
                auto_create_user: Some("autouser".into()),
                auto_create_user_pass: "other-pass".into(),
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert!(!report.auto_user_created);
            // The `--user-exists` guard: the existing password is untouched.
            assert!(
                users::user_check_password(db.pool(), "autouser", "original-pass")
                    .await
                    .unwrap()
            );
            let level: i32 =
                sqlx::query_scalar("SELECT access_level FROM ttrss_users WHERE login = $1")
                    .bind("autouser")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(level, 5);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_auto_user_enable_api() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            let vars = BootstrapVars {
                auto_create_user: Some("apiuser".into()),
                auto_create_user_pass: "pw".into(),
                auto_create_user_enable_api: Some("true".into()),
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert_eq!(report.auto_user_api, Some(true));
            let uid: i32 = sqlx::query_scalar("SELECT id FROM ttrss_users WHERE login = $1")
                .bind("apiuser")
                .fetch_one(db.pool())
                .await
                .unwrap();
            let pref: String = sqlx::query_scalar(
                "SELECT value FROM ttrss_user_prefs2
             WHERE pref_name = $1 AND owner_uid = $2 AND profile IS NULL",
            )
            .bind(ttrss::prefs::ENABLE_API_ACCESS)
            .bind(uid as i64)
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(pref, "1"); // bool true stores "1"

            // "false" → "" (bool false).
            let vars = BootstrapVars {
                auto_create_user: Some("apiuser".into()),
                auto_create_user_enable_api: Some("false".into()),
                ..Default::default()
            };
            let report = bootstrap::run(&db, &vars, data.path()).await.unwrap();
            assert_eq!(report.auto_user_api, Some(false));
            let pref: String = sqlx::query_scalar(
                "SELECT value FROM ttrss_user_prefs2
             WHERE pref_name = $1 AND owner_uid = $2 AND profile IS NULL",
            )
            .bind(ttrss::prefs::ENABLE_API_ACCESS)
            .bind(uid as i64)
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(pref, "");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── no-op ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_all_unset_is_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            // A non-stock admin password so the randomize step has nothing to do
            // (with the stock one, "no-op" would still randomize — by design).
            users::user_set_password(db.pool(), "admin", "custom-pass")
                .await
                .unwrap();
            let before = snapshot(&db).await;

            let report = bootstrap::run(&db, &BootstrapVars::default(), data.path())
                .await
                .unwrap();
            assert!(!report.admin_password_set);
            assert!(report.admin_password_randomized.is_none());
            assert!(!report.admin_level_set);
            assert!(!report.auto_user_created);
            assert_eq!(report.auto_user_api, None);

            assert_eq!(snapshot(&db).await, before);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── post-init SQL patches ───────────────────────────────────────────────────

fn write_patches(data: &tempfile::TempDir, files: &[(&str, &str)]) {
    let dir = data.path().join("sql").join("post-init.d");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
}

#[tokio::test]
async fn test_post_init_patches_applied_in_order() {
    with_test_db(|url| Box::pin(async move {
        let db = make_db(&url).await;
        let data = tempfile::tempdir().expect("data dir");
        write_patches(
            &data,
            &[
                // `a` creates the table, `b` inserts — the ordering guard:
                // sorted-by-name application (the reference's `find` order is
                // unspecified).
                (
                    "a_setup.sql",
                    "create table post_init_marker (note text); insert into post_init_marker values ('a');",
                ),
                (
                    "b_more.sql",
                    "insert into post_init_marker values ('b');",
                ),
                // Not a .sql file — the reference's `-name '*.sql'` skips it.
                ("c_notes.txt", "-- ignored"),
            ],
        );

        let report =
            bootstrap::run(&db, &BootstrapVars::default(), data.path()).await.unwrap();
        assert_eq!(report.post_init_applied, vec!["a_setup.sql", "b_more.sql"]);
        let notes: Vec<String> =
            sqlx::query_scalar("SELECT note FROM post_init_marker ORDER BY note")
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(notes, vec!["a".to_string(), "b".to_string()]);
        Ok(())
    }))
    .await
    .unwrap();
}

#[tokio::test]
async fn test_post_init_absent_dir_is_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            let report = bootstrap::run(&db, &BootstrapVars::default(), data.path())
                .await
                .unwrap();
            assert!(report.post_init_applied.is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_post_init_failing_patch_fails_closed() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = make_db(&url).await;
            let data = tempfile::tempdir().expect("data dir");
            write_patches(
                &data,
                &[(
                    "a_bad.sql",
                    "create table post_init_marker (note text); insert into nowhere values (1);",
                )],
            );
            let err = bootstrap::run(&db, &BootstrapVars::default(), data.path())
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("a_bad.sql"), "{err}");
            // The good statement committed (`psql -f` autocommit); the bad one
            // did not.
            let has_table: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = $1)",
            )
            .bind("post_init_marker")
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert!(has_table);
            Ok(())
        })
    })
    .await
    .unwrap();
}
