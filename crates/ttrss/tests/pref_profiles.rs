//! DB tests for the `Pref_Prefs` profile handlers (M4): `addprofile`,
//! `cloneprofile`, `remprofiles`, `activateprofile`, and `saveprofile`
//! (`getProfiles` is covered by the existing read-side tests).
//!
//! Rust-only: the frozen PHP suite has no `Pref_Prefs` configuration tests, so
//! these exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures. Owner 1 (the admin user in `schema.sql`) is the session uid.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

type Pool = sqlx::PgPool;

/// A test-only config (the handlers only need `db`).
fn test_config() -> Config { common::test_config() }

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Insert a settings profile owned by uid 1; return its id.
async fn insert_profile(db: &Pool, title: &str) -> i32 {
    sqlx::query_scalar(
        "INSERT INTO ttrss_settings_profiles (title, owner_uid) VALUES ($1, 1) RETURNING id",
    )
    .bind(title)
    .fetch_one(db)
    .await
    .expect("insert profile")
}

/// Insert a pref override row for uid 1 (profile `None` = the default profile).
async fn insert_pref(db: &Pool, pref_name: &str, value: &str, profile: Option<i32>) {
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, $1, $2, $3)",
    )
    .bind(pref_name)
    .bind(profile)
    .bind(value)
    .execute(db)
    .await
    .expect("insert pref");
}

/// Count uid 1's stored profiles.
async fn profile_count(db: &Pool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ttrss_settings_profiles WHERE owner_uid = 1")
        .fetch_one(db)
        .await
        .unwrap()
}

/// Count uid 1's pref overrides for a named profile.
async fn pref_count(db: &Pool, profile: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_user_prefs2 WHERE owner_uid = 1 AND profile = $1",
    )
    .bind(profile)
    .fetch_one(db)
    .await
    .unwrap()
}

/// Fetch a named profile's override value for a pref (None if absent).
async fn pref_value(db: &Pool, profile: i32, pref_name: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 \
         WHERE owner_uid = 1 AND profile = $1 AND pref_name = $2",
    )
    .bind(profile)
    .bind(pref_name)
    .fetch_optional(db)
    .await
    .unwrap()
}

/// The profile's title by id (or None).
async fn profile_title(db: &Pool, id: i32) -> Option<String> {
    sqlx::query_scalar("SELECT title FROM ttrss_settings_profiles WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .unwrap()
}

/// Insert a minimal user row (for the "foreign owner" tests; the `owner_uid`
/// FK requires a real user).
async fn insert_user(db: &Pool, id: i32, login: &str) {
    sqlx::query(
        "INSERT INTO ttrss_users (id, login, pwd_hash) \
         VALUES ($1, $2, 'SHA1:0000000000000000000000000000000000000000')",
    )
    .bind(id)
    .bind(login)
    .execute(db)
    .await
    .expect("insert user");
}

#[tokio::test]
async fn test_add_profile() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    assert_eq!(profile_count(db).await, 0);

    // Create a profile.
    ttrss::settings::add_profile(&state, &sess, &request(&[("title", json!("Work"))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 1);

    // Idempotent: adding the same title again does not create a duplicate.
    ttrss::settings::add_profile(&state, &sess, &request(&[("title", json!("Work"))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 1);

    // A distinct title creates a second profile.
    ttrss::settings::add_profile(&state, &sess, &request(&[("title", json!("Home"))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 2);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_add_profile_empty_title() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    // A whitespace-only (cleaned-to-empty) title is a no-op.
    ttrss::settings::add_profile(&state, &sess, &request(&[("title", json!("   "))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 0);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_save_profile() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    let id = insert_profile(db, "Old").await;

    // Rename the owned profile.
    ttrss::settings::save_profile(
        &state,
        &sess,
        &request(&[("id", json!(id)), ("value", json!("New"))]),
    )
    .await
    .unwrap();
    assert_eq!(profile_title(db, id).await.as_deref(), Some("New"));

    // A non-owned id is a no-op (the foreign profile keeps its title).
    let foreign_id = insert_profile(db, "Foreign").await;
    insert_user(db, 2, "user2").await;
    sqlx::query("UPDATE ttrss_settings_profiles SET owner_uid = 2 WHERE id = $1")
        .bind(foreign_id)
        .execute(db)
        .await
        .unwrap();
    ttrss::settings::save_profile(
        &state,
        &sess,
        &request(&[("id", json!(foreign_id)), ("value", json!("Hacked"))]),
    )
    .await
    .unwrap();
    assert_eq!(
        profile_title(db, foreign_id).await.as_deref(),
        Some("Foreign")
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_remove_profiles() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    let a = insert_profile(db, "A").await;
    let b = insert_profile(db, "B").await;
    // A pref row that must cascade-delete with profile A.
    insert_pref(db, "SHOW_DESCRIPTIONS", "0", Some(a)).await;

    ttrss::settings::remove_profiles(&state, &sess, &request(&[("ids", json!([a, b]))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 0);
    // The pref override cascade-deleted with its profile.
    assert!(pref_value(db, a, "SHOW_DESCRIPTIONS").await.is_none());
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_remove_profiles_keeps_active() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    let a = insert_profile(db, "A").await;
    let b = insert_profile(db, "B").await;
    // Make A the active profile (the session's profile).
    sess.with_mut(|s| s.set("profile", PValue::Int(a as i64)));

    // Request to remove both; A (active) must survive, B must go.
    ttrss::settings::remove_profiles(&state, &sess, &request(&[("ids", json!([a, b]))]))
        .await
        .unwrap();
    assert_eq!(profile_count(db).await, 1);
    assert_eq!(profile_title(db, a).await.as_deref(), Some("A"));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_activate_profile() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    let id = insert_profile(db, "Work").await;

    // Activate an owned profile → session profile set.
    ttrss::settings::activate_profile(&state, &sess, &request(&[("id", json!(id))])).await;
    assert_eq!(sess.data().profile(), Some(id as i64));

    // Activate a non-existent id → cleared (default).
    ttrss::settings::activate_profile(&state, &sess, &request(&[("id", json!(999999))])).await;
    assert_eq!(sess.data().profile(), None);

    // Activate a non-owned profile → cleared (default).
    let foreign = insert_profile(db, "Foreign").await;
    insert_user(db, 2, "user2").await;
    sqlx::query("UPDATE ttrss_settings_profiles SET owner_uid = 2 WHERE id = $1")
        .bind(foreign)
        .execute(db)
        .await
        .unwrap();
    ttrss::settings::activate_profile(&state, &sess, &request(&[("id", json!(foreign))])).await;
    assert_eq!(sess.data().profile(), None);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_clone_profile_from_default() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    // Default-profile (profile = NULL) overrides: one normal, one blacklisted.
    insert_pref(db, "HEADLINES_PER_PAGE", "50", None).await;
    insert_pref(db, "PURGE_OLD_DAYS", "30", None).await; // blacklisted

    let before = profile_count(db).await;
    ttrss::settings::clone_profile(
        &state,
        &sess,
        &request(&[("old_profile", json!("0")), ("new_title", json!("Clone"))]),
    )
    .await
    .unwrap();

    let new_id: i32 =
        sqlx::query_scalar("SELECT max(id) FROM ttrss_settings_profiles WHERE owner_uid = 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(profile_count(db).await, before + 1);
    // The non-blacklisted pref copied; the blacklisted one did not.
    assert_eq!(
        pref_value(db, new_id, "HEADLINES_PER_PAGE")
            .await
            .as_deref(),
        Some("50")
    );
    assert_eq!(pref_value(db, new_id, "PURGE_OLD_DAYS").await, None);
    assert_eq!(pref_count(db, new_id).await, 1);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_clone_profile_from_named() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    let src = insert_profile(db, "Source").await;
    // A named profile's overrides (including a blacklisted name — copied
    // verbatim when cloning from a named profile).
    insert_pref(db, "HEADLINES_PER_PAGE", "25", Some(src)).await;
    insert_pref(db, "PURGE_OLD_DAYS", "14", Some(src)).await;

    ttrss::settings::clone_profile(
        &state,
        &sess,
        &request(&[
            ("old_profile", json!(src.to_string())),
            ("new_title", json!("Copy")),
        ]),
    )
    .await
    .unwrap();

    let new_id: i32 = sqlx::query_scalar(
        "SELECT max(id) FROM ttrss_settings_profiles WHERE owner_uid = 1 AND id <> $1",
    )
    .bind(src)
    .fetch_one(db)
    .await
    .unwrap();
    // Both prefs copied (no blacklist applied when cloning from a named profile).
    assert_eq!(
        pref_value(db, new_id, "HEADLINES_PER_PAGE")
            .await
            .as_deref(),
        Some("25")
    );
    assert_eq!(
        pref_value(db, new_id, "PURGE_OLD_DAYS").await.as_deref(),
        Some("14")
    );
    assert_eq!(pref_count(db, new_id).await, 2);
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_clone_profile_bad_input() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // Non-digit old_profile → no-op (no new profile).
    ttrss::settings::clone_profile(
        &state,
        &sess,
        &request(&[("old_profile", json!("abc")), ("new_title", json!("X"))]),
    )
    .await
    .unwrap();
    assert_eq!(profile_count(db).await, 0);

    // Negative old_profile (ctype_digit false) → no-op.
    ttrss::settings::clone_profile(
        &state,
        &sess,
        &request(&[("old_profile", json!("-1")), ("new_title", json!("X"))]),
    )
    .await
    .unwrap();
    assert_eq!(profile_count(db).await, 0);

    // Empty new_title → no-op.
    ttrss::settings::clone_profile(
        &state,
        &sess,
        &request(&[("old_profile", json!("0")), ("new_title", json!("  "))]),
    )
    .await
    .unwrap();
    assert_eq!(profile_count(db).await, 0);
        Ok(())
    })).await.unwrap();
}
