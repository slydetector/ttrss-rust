//! DB tests for the `Pref_Users` admin write handlers (`edit`, `editSave`,
//! `remove`, `add`, `resetPass`) in `ttrss::users`. Rust-only: the
//! frozen PHP suite has no `Pref_Users` admin tests, so these exercise the
//! ported handlers on the versioned `schema` + `baseline` fixtures (owner 1 =
//! `admin`, access_level 10).
mod common;

use axum::body::to_bytes;
use axum::extract::{Extension, State};
use axum::http::{header, Method};
use serde_json::{Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::users;
use ttrss::AppState;

/// A test-only config (the handlers only need `db` + `config`).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        db_max_connections: 10,
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_max_jobs: 2,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
        docroot: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tt-rss"),
    }
}

/// Fresh fixtures + an admin session (`uid = 1`, `access_level = 10`).
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState {
        config: test_config(),
        db,
    };
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// A `POST` form field map from (name, value) pairs.
fn form(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

/// Seed a plain user (access_level 0) and return its id.
async fn seed_plain_user(pool: &sqlx::PgPool, login: &str) -> i32 {
    let (hash, salt) = test_db::ssha512_password(login, "password");
    test_db::seed_user(pool, login, &hash, &salt, 0)
        .await
        .expect("seed user")
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_edit(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    // Admin (uid 1) exists → the edit dialog data.
    let v = users::edit(&state, &sess, &form(&[("id", "1")])).await;
    let v = v.expect("edit admin");
    assert_eq!(v["user"]["id"], 1);
    assert_eq!(v["user"]["login"], "admin");
    assert_eq!(v["user"]["access_level"], 10);
    // The access-level name map is present with all five levels.
    assert_eq!(v["access_level_names"]["10"], "Administrator");
    assert_eq!(v["access_level_names"]["-2"], "Disabled");
    assert_eq!(v["access_level_names"]["0"], "User");

    // A missing user → `None`.
    assert!(users::edit(&state, &sess, &form(&[("id", "99999")]))
        .await
        .is_none());
}

/// Read a user's mutable fields back (login, access_level, email, otp).
async fn read_user(
    pool: &sqlx::PgPool,
    id: i32,
) -> Option<(String, i32, String, bool, Option<String>)> {
    sqlx::query_as(
        "SELECT login, access_level, email, otp_enabled, otp_secret \
         FROM ttrss_users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_edit_save_update(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();
    let id = seed_plain_user(pool, "TestUser").await;

    // Change access level + email; login is re-lowercased.
    let out = users::edit_save(
        &state,
        &sess,
        &form(&[
            ("id", &id.to_string()),
            ("login", "TestUser"),
            ("access_level", "5"),
            ("email", "new@example.com"),
        ]),
    )
    .await;
    assert_eq!(out, "", "no password posted → no message");

    let (login, level, email, _otp, _) = read_user(pool, id).await.expect("user still present");
    assert_eq!(login, "testuser", "login is lowercased");
    assert_eq!(level, 5);
    assert_eq!(email, "new@example.com");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_edit_save_admin_forced_admin_login(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();

    // id == 1 → the login is forced to "admin" regardless of the input.
    users::edit_save(
        &state,
        &sess,
        &form(&[
            ("id", "1"),
            ("login", "SuperRenamed"),
            ("access_level", "10"),
            ("email", "a@b.c"),
        ]),
    )
    .await;
    let (login, ..) = read_user(pool, 1).await.expect("admin present");
    assert_eq!(login, "admin", "admin login cannot be renamed");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_edit_save_empty_login_early_return(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();
    let id = seed_plain_user(pool, "keepme").await;

    // An empty login → early return (no save, no password reset) even though a
    // password was posted.
    let out = users::edit_save(
        &state,
        &sess,
        &form(&[
            ("id", &id.to_string()),
            ("login", ""),
            ("access_level", "5"),
            ("email", "changed@example.com"),
            ("password", "hunter2"),
        ]),
    )
    .await;
    assert_eq!(out, "", "early return → no message");
    let (login, level, email, ..) = read_user(pool, id).await.expect("user present");
    assert_eq!(login, "keepme", "login unchanged");
    assert_eq!(level, 0, "access level unchanged");
    assert_eq!(email, "", "email unchanged");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_edit_save_password_reset(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();
    let id = seed_plain_user(pool, "pwreset").await;
    let old_hash: String = sqlx::query_scalar("SELECT pwd_hash FROM ttrss_users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("old hash");

    let out = users::edit_save(
        &state,
        &sess,
        &form(&[
            ("id", &id.to_string()),
            ("login", "pwreset"),
            ("access_level", "0"),
            ("email", ""),
            ("password", "hunter2"),
        ]),
    )
    .await;
    assert_eq!(
        out,
        "Changed password of user <strong>pwreset</strong> to <strong>hunter2</strong>"
    );
    let new_hash: String = sqlx::query_scalar("SELECT pwd_hash FROM ttrss_users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("new hash");
    assert_ne!(old_hash, new_hash, "password hash changed");

    // The new password verifies against the stored hash.
    let salt: String = sqlx::query_scalar("SELECT salt FROM ttrss_users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("salt");
    assert!(ttrss::core::user::verify_password(
        &new_hash, "hunter2", &salt
    ));
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_remove(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();
    let victim = seed_plain_user(pool, "victim").await;

    // Remove a plain user (and it cascades to its data).
    users::remove(&state, &sess, &form(&[("ids", &victim.to_string())])).await;
    assert!(read_user(pool, victim).await.is_none(), "victim removed");

    // Admin (id 1) and the current session user (uid 1) are never removed.
    users::remove(&state, &sess, &form(&[("ids", "1")])).await;
    assert!(read_user(pool, 1).await.is_some(), "admin survives");

    // A non-existent id is a no-op.
    users::remove(&state, &sess, &form(&[("ids", "99999")])).await;
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_add(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let pool = state.db.pool();

    // A new user is created with a random password.
    let out = users::add(&state, &sess, &form(&[("login", "NewGuy")])).await;
    assert!(
        out.starts_with("Added user NewGuy with password "),
        "got: {out}"
    );
    // The password (the 12 chars after the prefix) is distinct-character.
    let pw = out
        .strip_prefix("Added user NewGuy with password ")
        .expect("prefix");
    assert_eq!(pw.len(), 12, "default password length is 12");
    // The user now exists, lowercased.
    let id: i32 = sqlx::query_scalar("SELECT id FROM ttrss_users WHERE LOWER(login) = 'newguy'")
        .fetch_one(pool)
        .await
        .expect("user created");
    let (login, level, ..) = read_user(pool, id).await.expect("row");
    assert_eq!(login, "newguy");
    assert_eq!(level, 0, "new users are access level 0");

    // A duplicate → the "already exists" message.
    let out = users::add(&state, &sess, &form(&[("login", "newguy")])).await;
    assert_eq!(out, "User newguy already exists.");

    // A blank login → empty (no user created, no message).
    let out = users::add(&state, &sess, &form(&[("login", "")])).await;
    assert_eq!(out, "");
}

#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_reset_pass(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    // Reset an existing user's password to a random one.
    let out = users::reset_pass(&state, &sess, &form(&[("id", "1")])).await;
    assert!(
        out.starts_with("Changed password of user <strong>admin</strong> to <strong>"),
        "got: {out}"
    );
    assert!(out.ends_with("</strong>"));

    // A missing user → "User not found".
    let out = users::reset_pass(&state, &sess, &form(&[("id", "99999")])).await;
    assert_eq!(out, "User not found");
}

/// The admin gate (`Handler_Administrative::before`): a `Pref_Users` write is
/// rejected for a non-admin session even when the csrf token is valid, and
/// proceeds for an admin. Exercised through the public `rpc::handler`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql"
))]
async fn test_admin_gate_denies_non_admin(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, _sess) = setup(pool, options).await;

    // A session for `uid = 1` (admin in the DB) carrying the admin's stored
    // pwd hash + a valid csrf token, so `validate_session` passes and the
    // *access-level* gate is what decides.
    fn sess_with_level(access_level: i64) -> Sess {
        let mut d = Session::new();
        d.set("uid", PValue::Int(1));
        d.set("access_level", PValue::Int(access_level));
        d.set(
            "pwd_hash",
            PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
        );
        d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
        d.set("name", PValue::Bytes(b"admin".to_vec()));
        d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
        let s = Sess::new("test-sid".to_string());
        s.replace("test-sid".to_string(), d);
        s
    }

    fn req(body: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(Method::POST)
            .uri("/backend.php")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    async fn call(state: &AppState, sess: Sess, body: &str) -> String {
        let res = ttrss::rpc::handler(State(state.clone()), Extension(sess), req(body)).await;
        let bytes = to_bytes(res.into_body(), 10 * 1024 * 1024)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    // Non-admin (access_level 0) → the gate rejects with E_UNAUTHORIZED.
    let s = call(
        &state,
        sess_with_level(0),
        "op=Pref_Users&method=add&csrf_token=tok&login=gateuser",
    )
    .await;
    let v: Value = serde_json::from_str(&s).expect("json");
    assert_eq!(
        v["error"]["code"], "E_UNAUTHORIZED",
        "non-admin rejected: {s}"
    );

    // Admin (access_level 10) → the gate passes and the user is created.
    let s = call(
        &state,
        sess_with_level(10),
        "op=Pref_Users&method=add&csrf_token=tok&login=gateuser",
    )
    .await;
    assert!(
        s.contains("Added user gateuser with password"),
        "admin proceeds: {s}"
    );
}
