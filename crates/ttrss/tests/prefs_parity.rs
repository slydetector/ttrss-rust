//! Test parity: 1:1 port of the DB-backed cases in
//! `tt-rss/tests_integration/PrefsTest.php` — the 14 cases the test-parity
//! program (docs/plan.md, row 3) parked as the `server::prefs` REVISIT
//! ledger. They exercise `Prefs::set` / `reset` / `get_all` (DB writes and
//! reads), now ported to `ttrss::prefs`.
//!
//! Runs on the fresh per-test database with the versioned `schema` +
//! `baseline` + `seed` fixtures. Each test creates its own synthetic user
//! (PHP `PrefsTest::createTestUser`), so the cases stay isolated exactly
//! like the PHP suite.

mod common;

use ttrss::core::db::Db;
use ttrss::prefs::{self, Pref, Type};

/// PHP: `PrefsTest::createTestUser` — a synthetic user with the
/// SHA1('password') hash and access level 0.
async fn create_test_user(db: &Db, login: &str) -> i64 {
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level)
         VALUES ($1, 'SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8', 0) RETURNING id",
    )
    .bind(login)
    .fetch_one(db.pool())
    .await
    .expect("create test user");
    id as i64
}

/// PHP: `PrefsTest::getRawPref` — the raw stored text (None when no row).
async fn get_raw_pref(
    db: &Db,
    pref_name: &str,
    owner_uid: i64,
    profile: Option<i32>,
) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM ttrss_user_prefs2
         WHERE pref_name = $1 AND owner_uid = $2
           AND (profile = $3 OR ($3 IS NULL AND profile IS NULL))",
    )
    .bind(pref_name)
    .bind(owner_uid)
    .bind(profile)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten()
}

async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> Db {
    common::make_db(pool, options).await.expect("test db")
}

// ── set() — basic ───────────────────────────────────────────────────────────

/// PHP: test_set_inserts_new_row_into_db
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_inserts_new_row_into_db(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_insert").await;
    let value = 120;

    let before = get_raw_pref(&db, "PURGE_OLD_DAYS", uid, None).await;
    assert!(before.is_none(), "No row should exist before set()");

    let result = prefs::set(&db, "PURGE_OLD_DAYS", &value.to_string(), uid, None, true).await;

    assert!(result, "set() should return true on success");
    assert_eq!(
        get_raw_pref(&db, "PURGE_OLD_DAYS", uid, None).await.as_deref(),
        Some("120"),
        "Value should be stored in the database"
    );
}

/// PHP: test_set_updates_existing_row
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_updates_existing_row(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_update").await;
    let value1 = 99;
    let value2 = 200;

    prefs::set(&db, "PURGE_OLD_DAYS", &value1.to_string(), uid, None, true).await;
    let first_row = get_raw_pref(&db, "PURGE_OLD_DAYS", uid, None).await;
    assert_eq!(first_row.as_deref(), Some("99"));

    prefs::set(&db, "PURGE_OLD_DAYS", &value2.to_string(), uid, None, true).await;
    assert_eq!(
        get_raw_pref(&db, "PURGE_OLD_DAYS", uid, None).await.as_deref(),
        Some("200"),
        "set() should UPDATE when row already exists"
    );
}

/// PHP: test_set_stores_bool_as_string_in_db
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_stores_bool_as_string_in_db(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_bool_db").await;

    // PHP passes the booleans `false`/`true`; the Rust port takes the string
    // form PHP coerces them to before casting ("" / "1").
    // ENABLE_FEED_CATS defaults to true, so set false first to force a DB row.
    prefs::set(&db, "ENABLE_FEED_CATS", "", uid, None, true).await;
    assert_eq!(
        get_raw_pref(&db, "ENABLE_FEED_CATS", uid, None).await.as_deref(),
        Some(""),
        "false should be stored as empty string in DB"
    );

    // Now set true — this is a change from the DB value.
    prefs::set(&db, "ENABLE_FEED_CATS", "1", uid, None, true).await;
    assert_eq!(
        get_raw_pref(&db, "ENABLE_FEED_CATS", uid, None).await.as_deref(),
        Some("1"),
        "true should be stored as \"1\" in DB"
    );
}

/// PHP: test_set_stores_string_in_db
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_stores_string_in_db(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_string_db").await;
    let value = "Y-m-d H:i";

    prefs::set(&db, "SHORT_DATE_FORMAT", value, uid, None, true).await;
    assert_eq!(
        get_raw_pref(&db, "SHORT_DATE_FORMAT", uid, None).await.as_deref(),
        Some(value)
    );
}

/// PHP: test_set_strips_tags_from_string_values
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_strips_tags_from_string_values(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_strip_tags").await;
    let malicious = "<script>alert('xss')</script>M d, G:i";

    prefs::set(&db, "SHORT_DATE_FORMAT", malicious, uid, None, true).await;

    // strip_tags removes the tags, trim removes surrounding whitespace.
    let rows = prefs::load_rows(&db, uid, None).await;
    assert_eq!(
        prefs::resolve("SHORT_DATE_FORMAT", &rows, None),
        Pref::Str("alert('xss')M d, G:i".to_string())
    );
}

/// PHP: test_set_returns_true_without_db_change_when_value_unchanged
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_returns_true_without_db_change_when_value_unchanged(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_noop").await;
    let value = 60; // This is the default

    // Setting the default value should still return true.
    let result = prefs::set(&db, "PURGE_OLD_DAYS", &value.to_string(), uid, None, true).await;

    assert!(
        result,
        "set() should return true even when value equals default"
    );

    // No row should have been inserted for the default value (because _get
    // returns the default from cache, and _set sees they're equal).
    let db_value = get_raw_pref(&db, "PURGE_OLD_DAYS", uid, None).await;
    assert!(
        db_value.is_none(),
        "No DB row should be created when setting the default value"
    );
}

// ── get_all() ───────────────────────────────────────────────────────────────

/// PHP: test_get_all_returns_all_pref_definitions
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_returns_all_pref_definitions(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_get_all").await;

    let rows = prefs::load_rows(&db, uid, None).await;
    let all = prefs::get_all(&rows, None);

    assert!(!all.is_empty());

    // (PHP asserts each entry has pref_name/value/type_hint keys; the Rust
    // (String, Pref, Type) tuple carries exactly those, so the structure is
    // guaranteed by the type.)

    // Check that all known pref definitions (PHP `_DEFAULTS`) are present.
    let found_names: Vec<&str> = all.iter().map(|(n, _, _)| n.as_str()).collect();
    for def in prefs::all_defs() {
        assert!(
            found_names.contains(&def.name),
            "Pref {} should be in get_all() result",
            def.name
        );
    }
}

/// PHP: test_get_all_returns_default_values_for_new_user
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_get_all_returns_default_values_for_new_user(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_get_all_defaults").await;

    let rows = prefs::load_rows(&db, uid, None).await;
    let all = prefs::get_all(&rows, None);

    // Find PURGE_OLD_DAYS in the result.
    let purge_old = all
        .iter()
        .find(|(n, _, _)| n == "PURGE_OLD_DAYS")
        .expect("PURGE_OLD_DAYS should be in get_all()");

    assert_eq!(purge_old.1, Pref::Int(60));
    assert_eq!(purge_old.2, Type::Int); // Config::T_INT
}

// ── profile blacklisting ────────────────────────────────────────────────────

/// PHP: test_set_profile_blacklisted_pref_returns_false
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_profile_blacklisted_pref_returns_false(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_profile_bl").await;
    let profile_id = 100;

    // PURGE_OLD_DAYS is in _PROFILE_BLACKLIST.
    let result = prefs::set(&db, "PURGE_OLD_DAYS", "999", uid, Some(profile_id), true).await;

    assert!(!result, "set() should return false for blacklisted pref with profile");
    assert!(
        get_raw_pref(&db, "PURGE_OLD_DAYS", uid, Some(profile_id)).await.is_none(),
        "Blacklisted pref should not be stored in the database"
    );
}

/// PHP: test_set_non_blacklisted_pref_works_with_profile
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_non_blacklisted_pref_works_with_profile(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_profile_nonbl").await;
    let profile_id = prefs::create_profile(&db, uid, "test_profile_nonbl").await;
    assert!(profile_id > 0, "create_profile should succeed");
    // REVERSE_HEADLINES is not in the blacklist.

    let result = prefs::set(&db, "REVERSE_HEADLINES", "1", uid, Some(profile_id as i32), true).await;

    assert!(result, "set() should succeed for non-blacklisted pref with profile");
    assert_eq!(
        get_raw_pref(&db, "REVERSE_HEADLINES", uid, Some(profile_id as i32)).await.as_deref(),
        Some("1")
    );
}

// ── reset() ─────────────────────────────────────────────────────────────────

/// PHP: test_reset_clears_all_user_prefs
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_reset_clears_all_user_prefs(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_reset").await;

    // Set some prefs.
    prefs::set(&db, "PURGE_OLD_DAYS", "99", uid, None, true).await;
    prefs::set(&db, "REVERSE_HEADLINES", "1", uid, None, true).await;
    prefs::set(&db, "USER_TIMEZONE", "America/New_York", uid, None, true).await;

    // Verify they exist.
    let count_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_prefs2 WHERE owner_uid = $1",
    )
    .bind(uid)
    .fetch_one(db.pool())
    .await
    .expect("count before");
    assert!(count_before >= 3);

    prefs::reset(&db, uid, None).await;

    // All prefs should be deleted except _PREFS_MIGRATED.
    let count_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_prefs2
         WHERE owner_uid = $1 AND pref_name != $2",
    )
    .bind(uid)
    .bind("_PREFS_MIGRATED")
    .fetch_one(db.pool())
    .await
    .expect("count after");
    assert_eq!(
        count_after, 0,
        "All prefs should be deleted by reset() except _PREFS_MIGRATED"
    );
}

/// PHP: test_reset_does_not_delete_migrated_flag
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_reset_does_not_delete_migrated_flag(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_reset_mig").await;

    // First, simulate migration by setting _PREFS_MIGRATED.
    prefs::set(&db, "_PREFS_MIGRATED", "1", uid, None, true).await;

    prefs::reset(&db, uid, None).await;

    let migrated = get_raw_pref(&db, "_PREFS_MIGRATED", uid, None).await;
    assert_eq!(
        migrated.as_deref(),
        Some("1"),
        "_PREFS_MIGRATED should not be deleted by reset()"
    );
}

// ── Edge cases ──────────────────────────────────────────────────────────────

/// PHP: test_set_returns_false_for_invalid_pref_name
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_set_returns_false_for_invalid_pref_name(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid = create_test_user(&db, "test_invalid").await;

    // PHP set() triggers user_error() for invalid pref names (suppressed in
    // the PHP test); the Rust port simply returns false.
    let result = prefs::set(&db, "INVALID_PREF_NAME", "value", uid, None, true).await;

    assert!(!result, "set() should return false for an invalid pref name");
}

/// PHP: test_multiple_users_have_isolated_prefs
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_multiple_users_have_isolated_prefs(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup(pool, options).await;
    let uid1 = create_test_user(&db, "test_iso_1").await;
    let uid2 = create_test_user(&db, "test_iso_2").await;

    prefs::set(&db, "PURGE_OLD_DAYS", "100", uid1, None, true).await;
    prefs::set(&db, "PURGE_OLD_DAYS", "200", uid2, None, true).await;

    let rows1 = prefs::load_rows(&db, uid1, None).await;
    let rows2 = prefs::load_rows(&db, uid2, None).await;
    assert_eq!(prefs::resolve("PURGE_OLD_DAYS", &rows1, None), Pref::Int(100));
    assert_eq!(prefs::resolve("PURGE_OLD_DAYS", &rows2, None), Pref::Int(200));
}
