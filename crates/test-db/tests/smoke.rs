//! Smoke test for the fixture set: verify the versioned fixture files
//! reproduce the PHP integration suite's starting state, applied by sqlx's
//! `#[sqlx::test]` harness exactly the way the parity suites use them.

#[ctor::ctor(unsafe)]
fn init_test_pg() {
    test_db::init_database_url();
}

async fn count(pool: &sqlx::PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(fixtures(
    "../fixtures/schema.sql",
    "../fixtures/baseline.sql",
    "../fixtures/seed.sql",
))]
async fn fixture_set_matches_php_integration_fixture(pool: sqlx::PgPool) {
    // baseline
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM ttrss_version")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, 151);

    // The admin user is created by schema.sql itself: password 'password'
    // (SHA1, unsalted), access_level 10 (ACCESS_LEVEL_ADMIN).
    let admin: (i32, i32, String) =
        sqlx::query_as("SELECT id, access_level, pwd_hash FROM ttrss_users WHERE login = 'admin'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(admin.0, 1);
    assert_eq!(admin.1, 10);
    assert_eq!(admin.2, "SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8");

    // seed.sql state
    assert_eq!(count(&pool, "ttrss_feed_categories").await, 2);
    assert_eq!(count(&pool, "ttrss_labels2").await, 2);
    assert_eq!(count(&pool, "ttrss_feeds").await, 4);
    assert_eq!(count(&pool, "ttrss_entries").await, 11);
    assert_eq!(count(&pool, "ttrss_user_entries").await, 11);
    let enclosures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_enclosures WHERE post_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(enclosures, 2);

    // article 1 is unread (from seed.sql)
    let unread: (bool,) =
        sqlx::query_as("SELECT unread FROM ttrss_user_entries WHERE ref_id = 1 AND owner_uid = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(unread.0);
}

#[sqlx::test(fixtures(
    "../fixtures/schema.sql",
    "../fixtures/baseline.sql",
    "../fixtures/seed.sql",
))]
async fn fixture_db_is_fresh_and_writable(pool: sqlx::PgPool) {
    // The per-test database starts from exactly the fixture state (no
    // leakage from other tests): the seed has 4 feeds, nothing else.
    assert_eq!(count(&pool, "ttrss_feeds").await, 4);
    let uid: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_users WHERE login = 'smokemark'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(uid, 0);

    // …and is writable (the parity suites mutate state within a test).
    let (hash, salt) = test_db::ssha512_password("smokemark", "smoke");
    let id: i32 = test_db::seed_user(&pool, "smokemark", &hash, &salt, 0)
        .await
        .unwrap();
    let row: (i32,) = sqlx::query_as("SELECT access_level FROM ttrss_users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, 0);
}
