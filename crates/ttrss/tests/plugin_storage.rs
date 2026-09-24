//! DB tests for plugin storage (M7.2): the 1:1 port of the "Plugin storage
//! (DB-backed)" section of `tt-rss/tests_integration/PluginHostTest.php`
//! (set/get, get-default, set_array, get_array, get_all, clear_data), plus
//! Rust-only owner/profile scoping and PHP byte-interop checks (the port's
//! `content` form must be exactly what PHP `serialize()` writes).
//!
//! Storage keys off the plugin **class name** (PHP's `$sender::class`), so
//! the test plugin's `class_name` is the row's `name`.

mod common;

use test_db::template::with_test_db;

use ttrss::core::phpser::{self, Key, Value as PValue};
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::{About, Plugin};
use ttrss::pluginhost::PluginHost;

/// Test plugin — its class name is the storage key (see the module docs).
struct TestStoragePlugin;

impl Plugin for TestStoragePlugin {
    fn name(&self) -> &'static str {
        "teststorage"
    }
    fn class_name(&self) -> &'static str {
        "TestStoragePlugin"
    }
    fn about(&self) -> About {
        About::new("test", "tester")
    }
    fn init(&self, _host: &mut PluginHost) {}
}

fn s(b: &[u8]) -> PValue {
    PValue::Bytes(b.to_vec())
}

fn bytes(k: &str, v: PValue) -> (Key, PValue) {
    (Key::Bytes(k.into()), v)
}

fn int(k: i64, v: PValue) -> (Key, PValue) {
    (Key::Int(k), v)
}

/// The versioned test DB (fresh template DB per test), as a pool the
/// storage methods can borrow.
async fn setup_pool(url: &str) -> sqlx::PgPool {
    let db = common::make_db_from_url(url).await.expect("test db");
    db.pool().clone()
}

/// `ttrss_plugin_storage.owner_uid` is FK to `ttrss_users`, and the fixtures
/// ship no users — seed one (the first inserted row gets id 1).
async fn seed_owner(pool: &sqlx::PgPool, login: &str) -> i32 {
    let (hash, salt) = test_db::ssha512_password(login, "password");
    test_db::seed_user(pool, login, &hash, &salt, 3).await.expect("seed user")
}

#[tokio::test]
async fn test_set_and_get() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_set_and_get
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    ctx.storage_set(&p, "key1", s(b"value1")).await.unwrap();

    assert_eq!(
        ctx.storage_get(&p, "key1", PValue::Bool(false)).await.unwrap(),
        s(b"value1")
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_returns_default_when_missing() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_get_returns_default_when_missing
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    assert_eq!(
        ctx.storage_get(&p, "missing_key", PValue::Bool(false)).await.unwrap(),
        PValue::Bool(false)
    );
    assert_eq!(
        ctx.storage_get(&p, "missing_key", s(b"default")).await.unwrap(),
        s(b"default")
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_set_array() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_set_array
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    ctx.storage_set_array(
        &p,
        &[
            ("a".to_string(), PValue::Int(1)),
            ("b".to_string(), PValue::Int(2)),
            ("c".to_string(), PValue::Int(3)),
        ],
    )
    .await
    .unwrap();

    assert_eq!(ctx.storage_get(&p, "a", PValue::Bool(false)).await.unwrap(), PValue::Int(1));
    assert_eq!(ctx.storage_get(&p, "b", PValue::Bool(false)).await.unwrap(), PValue::Int(2));
    assert_eq!(ctx.storage_get(&p, "c", PValue::Bool(false)).await.unwrap(), PValue::Int(3));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_array() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_get_array
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    ctx.storage_set(&p, "mylist", PValue::Array(vec![bytes("x", PValue::Int(10)), bytes("y", PValue::Int(20))]))
        .await
        .unwrap();

    assert_eq!(
        ctx.storage_get_array(&p, "mylist", PValue::Array(Vec::new()))
            .await
            .unwrap(),
        PValue::Array(vec![bytes("x", PValue::Int(10)), bytes("y", PValue::Int(20))])
    );

    // Non-array returns default.
    ctx.storage_set(&p, "notarray", s(b"string")).await.unwrap();
    assert_eq!(
        ctx.storage_get_array(&p, "notarray", PValue::Array(vec![bytes("default", PValue::Int(0))]))
            .await
            .unwrap(),
        PValue::Array(vec![bytes("default", PValue::Int(0))])
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_get_all() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_get_all
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    ctx.storage_set(&p, "k1", s(b"v1")).await.unwrap();
    ctx.storage_set(&p, "k2", s(b"v2")).await.unwrap();

    let all = ctx.storage_get_all(&p);
    assert_eq!(all.get("k1"), Some(&s(b"v1")));
    assert_eq!(all.get("k2"), Some(&s(b"v2")));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_clear_data_removes_from_db() {
    with_test_db(|url| Box::pin(async move {
    // PHP: test_clear_data_removes_from_db
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = TestStoragePlugin;

    ctx.storage_set(&p, "to_remove", s(b"value")).await.unwrap();
    ctx.storage_clear(&p).await.unwrap();

    assert_eq!(
        ctx.storage_get(&p, "to_remove", PValue::Bool(false)).await.unwrap(),
        PValue::Bool(false)
    );

    // Gone from the DB, not just the in-memory map.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ttrss_plugin_storage WHERE name = $1 AND owner_uid = $2")
            .bind(TestStoragePlugin.class_name())
            .bind(uid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
        Ok(())
    })).await.unwrap();
}

// ── Rust-only: scoping and interop ─────────────────────────────────────────

#[tokio::test]
async fn test_owner_isolation() {
    with_test_db(|url| Box::pin(async move {
    // (uid, class-name) keying: two owners, same plugin class.
    let pool = setup_pool(&url).await;
    let uid1 = seed_owner(&pool, "alice").await;
    let uid2 = seed_owner(&pool, "bob").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let p = TestStoragePlugin;

    let mut ctx1 = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid1), None, 0, &[], &[]);
    ctx1.storage_set(&p, "shared_key", s(b"alice-value")).await.unwrap();

    let mut ctx2 = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid2), None, 0, &[], &[]);
    assert_eq!(
        ctx2.storage_get(&p, "shared_key", PValue::Bool(false)).await.unwrap(),
        PValue::Bool(false)
    );
    // Owner 1's value is intact.
    assert_eq!(
        ctx1.storage_get(&p, "shared_key", PValue::Bool(false)).await.unwrap(),
        s(b"alice-value")
    );

    // A context with no owner never touches the DB (PHP's truthiness gate).
    let mut ctx0 = HostCtx::for_user(&host, &pool, &test_cfg, None, None, 0, &[], &[]);
    ctx0.storage_set(&p, "shared_key", s(b"orphan")).await.unwrap(); // no-op
    assert_eq!(
        ctx1.storage_get(&p, "shared_key", PValue::Bool(false)).await.unwrap(),
        s(b"alice-value")
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_profile_scoping() {
    with_test_db(|url| Box::pin(async move {
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;
    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let p = TestStoragePlugin;

    // No profile: profile_set/profile_get delegate to the plain set/get.
    let mut ctx_default = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    ctx_default
        .storage_profile_set(&p, "pref", s(b"plain"))
        .await
        .unwrap();
    assert_eq!(
        ctx_default
            .storage_profile_get(&p, "pref", PValue::Bool(false))
            .await
            .unwrap(),
        s(b"plain")
    );
    assert_eq!(
        ctx_default.storage_get(&p, "pref", PValue::Bool(false)).await.unwrap(),
        s(b"plain")
    );

    // A profile request that *loads first* (a prior get in the same request)
    // nests the profile value under its int-keyed slot, preserving the plain
    // key.
    let mut ctx_p1 = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), Some(1), 0, &[], &[]);
    assert_eq!(
        ctx_p1
            .storage_profile_get(&p, "other", PValue::Bool(false))
            .await
            .unwrap(),
        PValue::Bool(false)
    ); // (this load is what preserves the row below)
    ctx_p1
        .storage_profile_set(&p, "pref", s(b"profile-1"))
        .await
        .unwrap();
    assert_eq!(
        ctx_p1
            .storage_profile_get(&p, "pref", PValue::Bool(false))
            .await
            .unwrap(),
        s(b"profile-1")
    );
    assert_eq!(
        ctx_p1.storage_get(&p, "pref", PValue::Bool(false)).await.unwrap(),
        s(b"plain")
    );

    // PHP's documented quirk: a context with **no prior get** replaces the
    // whole row (profile_set does not call load_data) — the plain value is
    // clobbered, matching the reference.
    let mut ctx_p2 = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), Some(2), 0, &[], &[]);
    ctx_p2.storage_profile_set(&p, "only", s(b"v2")).await.unwrap();
    let fresh = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), Some(2), 0, &[], &[]);
    let mut fresh = fresh;
    assert_eq!(
        fresh
            .storage_profile_get(&p, "only", PValue::Bool(false))
            .await
            .unwrap(),
        s(b"v2")
    );
    assert_eq!(
        fresh.storage_get(&p, "pref", PValue::Bool(false)).await.unwrap(),
        PValue::Bool(false)
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_php_written_row_decode() {
    with_test_db(|url| Box::pin(async move {
    // Interop, read side: a row written with PHP's exact `serialize()` bytes
    // must decode through the storage API (byte-exact decode of PHP-written
    // fixture rows).
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;

    // PHP: serialize(['id' => 7, 'tags' => ['rss', 'xml'], 'enabled' => true])
    let php_bytes = b"a:3:{s:2:\"id\";i:7;s:4:\"tags\";a:2:{i:0;s:3:\"rss\";i:1;s:3:\"xml\";}s:7:\"enabled\";b:1;}";
    sqlx::query("INSERT INTO ttrss_plugin_storage (name, owner_uid, content) \
                 VALUES ($1, $2, $3)")
        .bind("PhpPlugin")
        .bind(uid)
        .bind(std::str::from_utf8(php_bytes).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let mut ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    let p = PhpPlugin;

    assert_eq!(ctx.storage_get(&p, "id", PValue::Bool(false)).await.unwrap(), PValue::Int(7));
    assert_eq!(
        ctx.storage_get(&p, "enabled", PValue::Bool(false)).await.unwrap(),
        PValue::Bool(true)
    );
    assert_eq!(
        ctx.storage_get_array(&p, "tags", PValue::Array(Vec::new()))
            .await
            .unwrap(),
        PValue::Array(vec![int(0, s(b"rss")), int(1, s(b"xml"))])
    );
        Ok(())
    })).await.unwrap();
}

struct PhpPlugin;
impl Plugin for PhpPlugin {
    fn name(&self) -> &'static str {
        "phpplugin"
    }
    fn class_name(&self) -> &'static str {
        "PhpPlugin"
    }
    fn about(&self) -> About {
        About::new("test", "tester")
    }
    fn init(&self, _host: &mut PluginHost) {}
}

#[tokio::test]
async fn test_encode_matches_php_bytes() {
    // Interop, write side: the `content` this port serializes must be
    // byte-identical to PHP's `serialize()` output (so PHP's `unserialize()`
    // reads the row unchanged).
    let v = PValue::Array(vec![
        bytes("tags", PValue::Array(vec![int(0, s(b"rss")), int(1, s(b"xml"))])),
        bytes("limit", PValue::Int(10)),
    ]);
    let php_bytes = b"a:2:{s:4:\"tags\";a:2:{i:0;s:3:\"rss\";i:1;s:3:\"xml\";}s:5:\"limit\";i:10;}";
    assert_eq!(phpser::serialize(&v), php_bytes.to_vec());
}

#[tokio::test]
async fn test_get_all_without_load() {
    with_test_db(|url| Box::pin(async move {
    // PHP's quirk: `get_all` returns the in-memory map without `load_data()` —
    // a fresh context (no prior get) sees an empty map even with a row.
    let pool = setup_pool(&url).await;
    let uid = seed_owner(&pool, "alice").await;

    sqlx::query("INSERT INTO ttrss_plugin_storage (name, owner_uid, content) \
                 VALUES ($1, $2, $3)")
        .bind("PhpPlugin")
        .bind(uid)
        .bind("a:1:{s:1:\"a\";s:1:\"b\";}")
        .execute(&pool)
        .await
        .unwrap();

    let host = PluginHost::new();
    let test_cfg = ttrss::core::config::Config::for_test();
    let p = PhpPlugin;
    let ctx = HostCtx::for_user(&host, &pool, &test_cfg, Some(uid), None, 0, &[], &[]);
    assert_eq!(ctx.storage_get_all(&p), PValue::Array(Vec::new()));
        Ok(())
    })).await.unwrap();
}
