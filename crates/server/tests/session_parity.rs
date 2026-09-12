//! Test parity: 1:1 port of `tt-rss/tests_integration/SessionsIntegrationTest.php`
//! (29 cases) plus the session-id-regeneration case deferred from row 8
//! (`UserHelperTest::test_authenticate_regenerates_session_id`).
//!
//! Starting state matches the PHP suite: fresh per-test database with the
//! versioned `schema` + `baseline` + `seed` fixtures (the `admin` user, uid 1,
//! comes from `schema.sql`), encryption off, single-user mode off — the
//! explicit config from `SessionsIntegrationTest::setUp`, expressed here as
//! the `key: None` / `single_user: false` arguments.
//!
//! The PHP class methods are ported to `ttrss_server::session::{exists, read,
//! write, destroy, validate_session}`; `read`/`write` operate on raw session
//! data strings exactly like PHP's `SessionHandlerInterface` methods.

mod common;

use base64::Engine as _;
use rand::RngCore;
use ttrss_server::core::db::Db;
use ttrss_server::core::phpser::Value;
use ttrss_server::core::session as core_session;
use ttrss_server::core::session::Session;
use ttrss_server::auth;
use ttrss_server::session as sessions;

/// `schema.sql`'s initial admin user password hash: SHA1("password").
const ADMIN_PWD_HASH: &str = "SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8";

/// `Crypt::generate_key()` — a fresh random 32-byte key.
fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

fn un_b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .expect("stored data is base64")
}

/// The raw `ttrss_sessions.data` value for `id` (PHP `getStoredData`).
async fn get_stored_data(db: &Db, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT data FROM ttrss_sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .expect("query stored data")
}

/// `setValidAdminSession`: uid=1 with the schema's admin pwd_hash.
fn valid_admin_session() -> Session {
    let mut s = Session::new();
    s.set("uid", Value::Int(1));
    s.set("pwd_hash", Value::Bytes(ADMIN_PWD_HASH.as_bytes().to_vec()));
    s
}

/// `setNonExistentUserSession`: uid=99999, no pwd_hash.
fn non_existent_user_session() -> Session {
    let mut s = Session::new();
    s.set("uid", Value::Int(99_999));
    s
}

/// `setGuestSession`: no uid, no pwd_hash.
fn guest_session() -> Session {
    Session::new()
}

/// `createDisabledUser`: a `disabled_test_user` with the known SHA1 hash and
/// `ACCESS_LEVEL_DISABLED` (-2); returns the session for it.
async fn disabled_user_session(db: &Db) -> Session {
    let uid: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_users (login, pwd_hash, access_level)
         VALUES ('disabled_test_user', $1, -2) RETURNING id",
    )
    .bind(ADMIN_PWD_HASH)
    .fetch_one(db.pool())
    .await
    .expect("insert disabled user");
    let mut s = Session::new();
    s.set("uid", Value::Int(uid as i64));
    s.set("pwd_hash", Value::Bytes(ADMIN_PWD_HASH.as_bytes().to_vec()));
    s
}

/// `UserHelperTest::setUp` on top of the harness-injected pool: a `testuser`
/// with the known SSHA-512 password `testpass123`.
async fn setup_with_testuser(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> Db {
    let db = common::make_db(pool, options).await.expect("test db");
    let (hash, salt) = ttrss_testing::ssha512_password("testuser", "testpass123");
    sqlx::query(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level)
         VALUES ('testuser', $1, $2, 0)",
    )
    .bind(&hash)
    .bind(&salt)
    .execute(db.pool())
    .await
    .expect("insert testuser");
    db
}

// ── exists() ────────────────────────────────────────────────────────────────

// PHP: test_exists_returns_false_for_nonexistent_session
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_exists_returns_false_for_nonexistent_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "nonexistent-session-id".to_string();

    assert!(
        !sessions::exists(db.pool(), &id).await,
        "exists() should return false for a session that has never been written"
    );
}

// PHP: test_exists_returns_true_after_write
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_exists_returns_true_after_write(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "exists-test-id";
    assert!(sessions::write(db.pool(), None, id, "test-data-for-exists").await);

    assert!(
        sessions::exists(db.pool(), id).await,
        "exists() should return true after a successful write"
    );
}

// ── write() ─────────────────────────────────────────────────────────────────

// PHP: test_write_stores_data_in_database
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_write_stores_data_in_database(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "write-store-test";
    let data = "hello world";

    assert!(sessions::write(db.pool(), None, id, data).await);

    let stored = get_stored_data(&db, id).await.expect("Session row should exist in database");
    assert_eq!(
        b64(data), stored,
        "Stored data should be base64-encoded version of the original"
    );
}

// PHP: test_write_overwrites_existing_session
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_write_overwrites_existing_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "write-overwrite-test";

    assert!(sessions::write(db.pool(), None, id, "original data").await);
    assert!(sessions::write(db.pool(), None, id, "updated data").await);

    let stored = get_stored_data(&db, id).await.expect("row exists");
    assert_eq!(
        b64("updated data"), stored,
        "Second write should overwrite the first"
    );
}

// ── read() ──────────────────────────────────────────────────────────────────

// PHP: test_read_returns_written_data
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_returns_written_data(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "read-roundtrip";
    let data = "session payload";

    assert!(sessions::write(db.pool(), None, id, data).await);
    let result = sessions::read(db.pool(), None, id).await;

    assert_eq!(result.as_deref(), Some(data), "read() should return the exact data that was written");
}

// PHP: test_read_returns_empty_string_for_new_session
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_returns_empty_string_for_new_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "read-auto-create";

    let result = sessions::read(db.pool(), None, id).await;

    assert_eq!(
        result.as_deref(),
        Some(""),
        "read() for a new session should return empty string"
    );
    assert!(
        sessions::exists(db.pool(), id).await,
        "read() should auto-create the session in the database"
    );
}

// PHP: test_read_with_unicode_data
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_with_unicode_data(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "read-unicode";
    let data = "こんにちは世界 🌍 Привет мир 你好世界";

    assert!(sessions::write(db.pool(), None, id, data).await);
    let result = sessions::read(db.pool(), None, id).await;

    assert_eq!(result.as_deref(), Some(data), "Unicode data should roundtrip correctly");
}

// PHP: test_read_with_large_payload
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_with_large_payload(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "read-large";
    let data = "A".repeat(50_000); // 50 KB

    assert!(sessions::write(db.pool(), None, id, &data).await);
    let result = sessions::read(db.pool(), None, id).await;

    assert_eq!(result.as_deref(), Some(data.as_str()), "Large payload should roundtrip correctly");
}

// PHP: test_read_returns_false_on_failure
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_returns_false_on_failure(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "read-type-test";

    // Auto-create returns '' (string), not false.
    let result = sessions::read(db.pool(), None, id).await;
    assert!(result.is_some(), "read() should return a string (even if empty)");
}

// ── destroy() ───────────────────────────────────────────────────────────────

// PHP: test_destroy_removes_session_from_database
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_destroy_removes_session_from_database(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "destroy-test";

    assert!(sessions::write(db.pool(), None, id, "to-be-destroyed").await);
    assert!(sessions::exists(db.pool(), id).await, "Session should exist before destroy");

    assert!(sessions::destroy(db.pool(), id).await, "destroy() should return true on success");
    assert!(
        !sessions::exists(db.pool(), id).await,
        "Session should not exist after destroy"
    );
}

// PHP: test_destroy_nonexistent_session_returns_true
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_destroy_nonexistent_session_returns_true(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "destroy-missing";

    // destroy() uses DELETE which returns true even if 0 rows affected
    assert!(
        sessions::destroy(db.pool(), id).await,
        "destroy() should return true even for non-existent sessions"
    );
}

// ── Full lifecycle ──────────────────────────────────────────────────────────

// PHP: test_full_session_lifecycle
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_full_session_lifecycle(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "lifecycle";

    // 1. New session does not exist
    assert!(!sessions::exists(db.pool(), id).await);

    // 2. read() auto-creates an empty session
    assert_eq!(sessions::read(db.pool(), None, id).await.as_deref(), Some(""));
    assert!(sessions::exists(db.pool(), id).await);

    // 3. write() stores data
    assert!(sessions::write(db.pool(), None, id, "step-3-data").await);
    assert_eq!(sessions::read(db.pool(), None, id).await.as_deref(), Some("step-3-data"));

    // 4. write() updates existing data
    assert!(sessions::write(db.pool(), None, id, "step-4-data").await);
    assert_eq!(sessions::read(db.pool(), None, id).await.as_deref(), Some("step-4-data"));

    // 5. destroy() removes the session
    assert!(sessions::destroy(db.pool(), id).await);
    assert!(!sessions::exists(db.pool(), id).await);
}

// ── Encryption ──────────────────────────────────────────────────────────────

// PHP: test_write_stores_encrypted_data_when_key_configured
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_write_stores_encrypted_data_when_key_configured(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-write";
    let data = "sensitive session data";

    assert!(sessions::write(db.pool(), Some(&key), id, data).await);

    let stored_b64 = get_stored_data(&db, id).await.expect("Session row should exist in database");
    let raw_stored = un_b64(&stored_b64);

    // With encryption, data is: serialize(Crypt::encrypt_string($data))
    // which produces a serialized PHP array — not the plaintext base64
    assert_ne!(b64(data), stored_b64, "Stored data should NOT be the plaintext base64 encoding");

    // The raw stored data should be a serialized PHP structure
    let unserialized = ttrss_server::core::phpser::deserialize(&raw_stored).expect("stored data should be a serialized PHP array (encrypted object)");
    assert!(unserialized.as_array().is_some(), "Stored data should be a serialized PHP array (encrypted object)");
    assert!(unserialized.get("algo").is_some());
    assert!(unserialized.get("nonce").is_some());
    assert!(unserialized.get("payload").is_some());
}

// PHP: test_write_stores_plaintext_when_encryption_disabled
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_write_stores_plaintext_when_encryption_disabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    // Encryption is disabled by default (PHP setUp)
    let id = "plaintext-write";
    let data = "not sensitive data";

    assert!(sessions::write(db.pool(), None, id, data).await);

    let stored_b64 = get_stored_data(&db, id).await.expect("row exists");
    assert_eq!(
        b64(data), stored_b64,
        "Stored data should be the plaintext base64 encoding when encryption is off"
    );
}

// PHP: test_read_roundtrip_with_encryption_enabled
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_roundtrip_with_encryption_enabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-roundtrip";
    let data = "secret session payload";

    assert!(sessions::write(db.pool(), Some(&key), id, data).await);
    let result = sessions::read(db.pool(), Some(&key), id).await;

    assert_eq!(result.as_deref(), Some(data), "Encrypted data should roundtrip correctly");
}

// PHP: test_read_roundtrip_with_encryption_disabled
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_roundtrip_with_encryption_disabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    // Encryption is disabled by default (PHP setUp)
    let id = "plaintext-roundtrip";
    let data = "open session data";

    assert!(sessions::write(db.pool(), None, id, data).await);
    let result = sessions::read(db.pool(), None, id).await;

    assert_eq!(result.as_deref(), Some(data), "Plaintext data should roundtrip correctly");
}

// PHP: test_read_with_encryption_handles_empty_data
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_with_encryption_handles_empty_data(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-empty";

    // write empty string
    assert!(sessions::write(db.pool(), Some(&key), id, "").await);
    let result = sessions::read(db.pool(), Some(&key), id).await;

    assert_eq!(result.as_deref(), Some(""), "Empty string should roundtrip with encryption");
}

// PHP: test_read_with_encryption_handles_unicode
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_with_encryption_handles_unicode(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-unicode";
    let data = "秘密情報 🔐 安全なセッション";

    assert!(sessions::write(db.pool(), Some(&key), id, data).await);
    let result = sessions::read(db.pool(), Some(&key), id).await;

    assert_eq!(result.as_deref(), Some(data), "Unicode data should roundtrip with encryption");
}

// PHP: test_read_with_encryption_handles_large_payload
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_read_with_encryption_handles_large_payload(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-large";
    let data = "S".repeat(50_000); // 50 KB

    assert!(sessions::write(db.pool(), Some(&key), id, &data).await);
    let result = sessions::read(db.pool(), Some(&key), id).await;

    assert_eq!(result.as_deref(), Some(data.as_str()), "Large payload should roundtrip with encryption");
}

// PHP: test_different_encryption_keys_produce_different_storage
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_different_encryption_keys_produce_different_storage(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let id = "key-diff";
    let data = "same plaintext";

    // Write with key1
    let key1 = random_key();
    assert!(sessions::write(db.pool(), Some(&key1), id, data).await);
    let stored1 = get_stored_data(&db, id).await.expect("row exists");

    // Destroy and re-write with key2
    assert!(sessions::destroy(db.pool(), id).await);

    let key2 = random_key();
    assert!(sessions::write(db.pool(), Some(&key2), id, data).await);
    let stored2 = get_stored_data(&db, id).await.expect("row exists");

    assert_ne!(stored1, stored2, "Different keys should produce different stored data");

    // But both should decrypt correctly
    assert!(sessions::destroy(db.pool(), id).await); // clean up the key2 entry
    assert!(sessions::write(db.pool(), Some(&key1), id, data).await); // re-write with key1
    let result = sessions::read(db.pool(), Some(&key1), id).await;
    assert_eq!(result.as_deref(), Some(data));
}

// PHP: test_encrypt_then_decrypt_with_key_removed_fails_gracefully
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_encrypt_then_decrypt_with_key_removed_fails_gracefully(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let key = random_key();
    let id = "encrypt-stale";
    let data = "encrypted before key removal";

    // Write with encryption enabled
    assert!(sessions::write(db.pool(), Some(&key), id, data).await);

    // Now remove the encryption key (PHP: Config::set(ENCRYPTION_KEY, ''))
    // Reading without the key: read() falls back to returning the raw
    // base64-decoded data (which is the serialized encrypted blob).
    let result = sessions::read(db.pool(), None, id).await;

    // The result should NOT be the original plaintext — it's the fallback path
    assert_ne!(result.as_deref(), Some(data));

    // The data should still exist in the DB
    assert!(sessions::exists(db.pool(), id).await);
}

// ── validate_session() ──────────────────────────────────────────────────────

// PHP: test_validate_session_returns_true_in_single_user_mode
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_returns_true_in_single_user_mode(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");

    // Session state doesn't matter in single-user mode
    let mut s = guest_session();

    assert!(
        sessions::validate_session(db.pool(), true, &mut s).await,
        "Should always return true when SINGLE_USER_MODE is enabled"
    );
}

// PHP: test_validate_session_returns_true_for_guest
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_returns_true_for_guest(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = guest_session();

    assert!(
        sessions::validate_session(db.pool(), false, &mut s).await,
        "Guest sessions (no uid) should validate successfully"
    );
}

// PHP: test_validate_session_returns_true_for_valid_admin
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_returns_true_for_valid_admin(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = valid_admin_session();

    assert!(
        sessions::validate_session(db.pool(), false, &mut s).await,
        "Admin session with correct pwd_hash should validate"
    );
}

// PHP: test_validate_session_fails_when_password_changed
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_fails_when_password_changed(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = valid_admin_session();

    // Corrupt the pwd_hash in the session
    s.set("pwd_hash", Value::Bytes(b"SHA1:wronghashvalue000000000000000000000000000000".to_vec()));

    let result = sessions::validate_session(db.pool(), false, &mut s).await;

    assert!(!result, "Should fail when pwd_hash does not match DB");
    let msg = s.get_str("login_error_msg").expect("Should set login_error_msg on password mismatch");
    assert!(
        msg.contains("password changed"),
        "Error message should mention password change"
    );
}

// PHP: test_validate_session_fails_when_user_not_found
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_fails_when_user_not_found(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = non_existent_user_session();

    let result = sessions::validate_session(db.pool(), false, &mut s).await;

    assert!(!result, "Should fail when user ID does not exist in DB");
    let msg = s.get_str("login_error_msg").expect("Should set login_error_msg when user not found");
    assert!(
        msg.contains("not found"),
        "Error message should mention user not found"
    );
}

// PHP: test_validate_session_fails_when_account_disabled
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_fails_when_account_disabled(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = disabled_user_session(&db).await;

    let result = sessions::validate_session(db.pool(), false, &mut s).await;

    assert!(!result, "Should fail when account is disabled");
    let msg = s.get_str("login_error_msg").expect("Should set login_error_msg for disabled account");
    assert!(
        msg.contains("disabled"),
        "Error message should mention account is disabled"
    );
}

// PHP: test_validate_session_with_empty_pwd_hash_in_session
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_with_empty_pwd_hash_in_session(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = valid_admin_session();

    // Set pwd_hash to empty string (but uid is valid)
    s.set("pwd_hash", Value::Bytes(Vec::new()));

    let result = sessions::validate_session(db.pool(), false, &mut s).await;

    assert!(
        !result,
        "Should fail when pwd_hash in session is empty but user exists in DB"
    );
}

// PHP: test_validate_session_with_uid_zero
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_validate_session_with_uid_zero(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = common::make_db(pool, options).await.expect("test db");
    let mut s = Session::new();
    // uid=0 is falsy in PHP, so it should be treated as a guest
    s.set("uid", Value::Int(0));

    let result = sessions::validate_session(db.pool(), false, &mut s).await;

    assert!(
        result,
        "uid=0 (falsy) should be treated as guest and validate successfully"
    );
}

// ── Session-id regeneration (deferred from row 8) ───────────────────────────

// PHP: UserHelperTest::test_authenticate_regenerates_session_id
// (ported here, parity row 9: the id change is a session-layer effect of a
// successful login — PHP `session_regenerate_id(true)`)
#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql",
    "../../testing/fixtures/seed.sql",
))]
async fn test_authenticate_regenerates_session_id(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let db = setup_with_testuser(pool, options).await;

    let before = core_session::new_session_id();
    let auth::AuthOutcome::Success {
        id,
        login,
        access_level,
        pwd_hash,
    } = auth::authenticate(&db, "testuser", "testpass123", None).await
    else {
        panic!("expected success");
    };

    let (after, sess) =
        auth::build_login_session(id, &login, access_level, &pwd_hash, false, false);

    assert_ne!(after, before, "session id should change after a successful login");
    // PHP session.sid_length=32, sid_bits_per_character=4 (hex)
    assert_eq!(after.len(), 32);
    assert_eq!(sess.uid(), Some(id as i64));
}
