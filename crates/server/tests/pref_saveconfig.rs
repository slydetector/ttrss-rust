//! DB tests for the `Pref_Prefs` prefs-save/reset handlers (M4):
//! `saveconfig` (persist the prefs form) and `resetconfig`
//! (`Prefs::reset`).
//!
//! Rust-only: the frozen PHP suite has no `Pref_Prefs` configuration tests, so
//! these exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures. Owner 1 (the admin user in `schema.sql`) is the session uid.
mod common;

use serde_json::{json, Map, Value};
use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

type Pool = sqlx::PgPool;

/// A test-only config (the handlers only need `db`).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        forbid_password_changes: false,
        disable_login_form: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
    }
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
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
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// The default-profile (profile IS NULL) override value for a pref (None if
/// the override row is absent).
async fn pref_value(db: &Pool, pref_name: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT value FROM ttrss_user_prefs2 \
         WHERE owner_uid = 1 AND pref_name = $1 AND profile IS NULL",
    )
    .bind(pref_name)
    .fetch_optional(db)
    .await
    .unwrap()
}

/// Count uid 1's default-profile pref overrides.
async fn pref_count(db: &Pool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_user_prefs2 \
         WHERE owner_uid = 1 AND profile IS NULL",
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// `last_digest_sent` for uid 1 (None = NULL).
async fn last_digest_sent(db: &Pool) -> Option<chrono::NaiveDateTime> {
    sqlx::query_scalar("SELECT last_digest_sent FROM ttrss_users WHERE id = 1")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_sets_prefs(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    let msg = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[
            ("COMBINED_DISPLAY_MODE", json!("false")),
            ("USER_STYLESHEET", json!("body { color: red; }")),
        ]),
    )
    .await;

    // A changed Bool pref is stored as its string form (false -> "").
    assert_eq!(
        pref_value(db, "COMBINED_DISPLAY_MODE").await.as_deref(),
        Some("")
    );
    // A Str pref is stored verbatim.
    assert_eq!(
        pref_value(db, "USER_STYLESHEET").await.as_deref(),
        Some("body { color: red; }")
    );
    assert_eq!(msg, "The configuration was saved.");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_boolean_default(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // `boolean_prefs` lists a pref the form did not submit; it defaults to
    // "false" and is stored.
    let _ = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[
            (
                "boolean_prefs",
                json!("SHOW_CONTENT_PREVIEW,HIDE_READ_FEEDS"),
            ),
            ("USER_STYLESHEET", json!("x")),
        ]),
    )
    .await;

    assert_eq!(
        pref_value(db, "SHOW_CONTENT_PREVIEW").await.as_deref(),
        Some("")
    );
    // `HIDE_READ_FEEDS` (default false) set to false is a no-op in `set` (no
    // override row written), so it resolves to the default.
    assert_eq!(pref_value(db, "HIDE_READ_FEEDS").await, None);
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_ignores_invalid_pref(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // A made-up pref name is not a valid pref key, so it is not stored.
    let _ = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[
            ("NOT_A_PREF", json!("value")),
            ("USER_STYLESHEET", json!("a")),
        ]),
    )
    .await;

    assert_eq!(pref_value(db, "NOT_A_PREF").await, None);
    assert_eq!(
        pref_value(db, "USER_STYLESHEET").await.as_deref(),
        Some("a")
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_blacklisted_tags_normalized(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // `BLACKLISTED_TAGS` is normalized: lowercased, numeric tags get the
    // `t:` prefix, de-duplicated, sorted ascending, joined with ", ".
    let _ = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[("BLACKLISTED_TAGS", json!("News, news, MAIN, 5, main"))]),
    )
    .await;

    assert_eq!(
        pref_value(db, "BLACKLISTED_TAGS").await.as_deref(),
        Some("main, news, t:5")
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_need_reload_on_theme_change(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // A changed `USER_CSS_THEME` (default "") triggers a reload.
    let msg = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[("USER_CSS_THEME", json!("dark"))]),
    )
    .await;

    assert_eq!(msg, "PREFS_NEED_RELOAD");
    assert_eq!(
        pref_value(db, "USER_CSS_THEME").await.as_deref(),
        Some("dark")
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_no_reload_unchanged(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;

    // Saving an unchanged `USER_CSS_THEME` (still the default "") does not
    // trigger a reload.
    let msg = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[("USER_CSS_THEME", json!(""))]),
    )
    .await;

    assert_eq!(msg, "The configuration was saved.");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_saveconfig_digest_time_resets(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // Seed a non-NULL digest time.
    sqlx::query("UPDATE ttrss_users SET last_digest_sent = '2024-01-01 00:00:00' WHERE id = 1")
        .execute(db)
        .await
        .unwrap();
    assert!(last_digest_sent(db).await.is_some());

    // A changed `DIGEST_PREFERRED_TIME` (default "00:00") clears it.
    let _ = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[("DIGEST_PREFERRED_TIME", json!("05:00"))]),
    )
    .await;

    assert!(last_digest_sent(db).await.is_none());
    assert_eq!(
        pref_value(db, "DIGEST_PREFERRED_TIME").await.as_deref(),
        Some("05:00")
    );
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_resetconfig_discards_overrides(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // Seed some overrides (plus `_PREFS_MIGRATED`, which `Prefs::reset` keeps).
    let _ = ttrss_server::settings::saveconfig(
        &state,
        &sess,
        &request(&[
            ("SHOW_CONTENT_PREVIEW", json!("false")),
            ("USER_STYLESHEET", json!("x")),
        ]),
    )
    .await;
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, '_PREFS_MIGRATED', NULL, '1')",
    )
    .execute(db)
    .await
    .unwrap();
    assert!(pref_count(db).await >= 3);

    let msg = ttrss_server::settings::resetconfig(&state, &sess).await;
    assert_eq!(msg, "PREFS_NEED_RELOAD");

    // The overrides are gone; `_PREFS_MIGRATED` survives.
    assert_eq!(pref_value(db, "SHOW_CONTENT_PREVIEW").await, None);
    assert_eq!(pref_value(db, "USER_STYLESHEET").await, None);
    assert_eq!(
        pref_value(db, "_PREFS_MIGRATED").await.as_deref(),
        Some("1")
    );
}
