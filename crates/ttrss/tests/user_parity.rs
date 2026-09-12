//! Test parity: 1:1 port of `tt-rss/tests_integration/UserHelperTest.php`
//! (8 cases, auth via the `auth_internal` plugin in multi-user mode).
//!
//! Starting state, matching the PHP suite:
//!
//! * the PHP runner boots a fresh Postgres, runs `update.php
//!   --update-schema=force-yes` (base `schema.sql` + `ttrss_version` = 151;
//!   the initial `admin` user is created by `schema.sql` itself), and
//!   `DbTestCase::setUp` re-applies `seed.sql` per test;
//! * `UserHelperTest::setUp` then inserts a `testuser` with a known
//!   SSHA-512 password hash.
//!
//! The Rust side gets the same state from the `#[sqlx::test]` harness (a
//! fresh per-test database with the versioned `schema` + `baseline` + `seed`
//! fixtures applied, in order) plus the same user insert.
//!
//! The PHP suite runs with `TTRSS_SINGLE_USER_MODE` unset (multi-user mode);
//! the Rust server only implements the multi-user `authenticate` path, so the
//! conditions match.

mod common;

use ttrss::core::db::Db;
use ttrss::core::user;
use ttrss::auth::{self, AuthOutcome};

/// `UserHelperTest::setUp` on top of the harness-injected pool: a `testuser`
/// with the known SSHA-512 password `testpass123` (fresh salt, as in PHP).
/// Returns the DB and the new user's id.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (Db, i32) {
    let db = common::make_db(pool, options).await.expect("test db");
    let (hash, salt) = ttrss_testing::ssha512_password("testuser", "testpass123");
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level)
         VALUES ($1, $2, $3, 0)
         RETURNING id",
    )
    .bind("testuser")
    .bind(&hash)
    .bind(&salt)
    .fetch_one(db.pool())
    .await
    .expect("insert testuser");
    (db, uid)
}

fn expect_success(outcome: AuthOutcome) -> (i32, String, i32) {
    match outcome {
        AuthOutcome::Success {
            id,
            login,
            access_level,
            ..
        } => (id, login, access_level),
        other => panic!("expected success, got {other:?}"),
    }
}

fn expect_failure(outcome: AuthOutcome) -> String {
    match outcome {
        AuthOutcome::Failure { message } => message,
        other => panic!("expected failure, got {other:?}"),
    }
}

// ── Happy paths ──────────────────────────────────────────────────────────────

// PHP: test_correct_credentials_returns_true_and_sets_session
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_correct_credentials_returns_true_and_sets_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, uid) = setup(pool, options).await;

    let (id, login, access_level) =
        expect_success(auth::authenticate(&db, "testuser", "testpass123", None).await);

    // PHP asserts $_SESSION['uid'], $_SESSION['name'], and
    // $_SESSION['access_level'] == UserHelper::ACCESS_LEVEL_USER (0).
    assert_eq!(id, uid);
    assert_eq!(login, "testuser");
    assert_eq!(access_level, 0);
    // NOTE: PHP also asserts $_SESSION['auth_module'] == 'auth_internal'; the
    // Rust handler layer (main.rs login_flow) sets that session key from the
    // same Success value — asserted at the handler/session layer (parity row
    // 9, SessionsIntegrationTest).
}

// PHP: test_admin_credentials_work
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_admin_credentials_work(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, _) = setup(pool, options).await;

    let (id, _login, access_level) =
        expect_success(auth::authenticate(&db, "admin", "password", None).await);

    // PHP asserts uid == 1 and access_level == UserHelper::ACCESS_LEVEL_ADMIN.
    assert_eq!(id, 1);
    assert_eq!(access_level, 10);
    // NOTE: the `admin` row comes from `schema.sql` itself (password
    // 'password', SHA1 hash, access_level 10). PHP's check_password upgrades
    // the stored hash to SSHA-512 on success; the Rust port does the same in
    // `auth::authenticate` (the session receives the upgraded hash).
}

// PHP: test_authenticate_regenerates_session_id
// Ported in `session_parity.rs` (parity row 9): the id change is a
// session-layer effect (`session_regenerate_id(true)`), asserted there on
// `auth::build_login_session`.

// PHP: test_authenticate_updates_last_login
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_authenticate_updates_last_login(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, _) = setup(pool, options).await;

    // PHP: usleep(100_000) before authenticating.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    expect_success(auth::authenticate(&db, "testuser", "testpass123", None).await);

    let last_login: Option<chrono::NaiveDateTime> = sqlx::query_scalar(
        "SELECT last_login FROM ttrss_users WHERE login = 'testuser'",
    )
    .fetch_one(db.pool())
    .await
    .expect("query last_login");

    assert!(last_login.is_some());
}

// ── Failures ─────────────────────────────────────────────────────────────────

// PHP: test_wrong_password_returns_false
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_wrong_password_returns_false(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, _) = setup(pool, options).await;

    let _msg = expect_failure(auth::authenticate(&db, "testuser", "wrong", None).await);
}

// PHP: test_nonexistent_user_returns_false
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_nonexistent_user_returns_false(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, _) = setup(pool, options).await;

    let _msg = expect_failure(auth::authenticate(&db, "ghost", "anything", None).await);
}

// PHP: test_login_is_case_insensitive
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_login_is_case_insensitive(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, uid) = setup(pool, options).await;

    let (id, _login, _access_level) =
        expect_success(auth::authenticate(&db, "TESTUSER", "testpass123", None).await);

    assert_eq!(id, uid);
}

// ── Disabled user ────────────────────────────────────────────────────────────

// PHP: test_disabled_user_cannot_authenticate
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_disabled_user_cannot_authenticate(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (db, _) = setup(pool, options).await;

    // PHP inserts: hash_password('nope', 'saltxx', HASH_ALGO_SSHA512),
    // access_level -2 (ACCESS_LEVEL_DISABLED).
    let hash = user::hash_password("nope", "saltxx", user::ALGO_SSHA512).unwrap();
    sqlx::query(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level)
         VALUES ('disabled', $1, 'saltxx', -2)",
    )
    .bind(&hash)
    .execute(db.pool())
    .await
    .expect("insert disabled user");

    let _msg = expect_failure(auth::authenticate(&db, "disabled", "nope", None).await);
}
