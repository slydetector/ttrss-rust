//! DB tests for the `Pref_Prefs` prefs-save/reset handlers (M4):
//! `saveconfig` (persist the prefs form) and `resetconfig`
//! (`Prefs::reset`).
//!
//! Rust-only: the frozen PHP suite has no `Pref_Prefs` configuration tests, so
//! these exercise the ported handlers on the versioned `schema` + `baseline`
//! fixtures. Owner 1 (the admin user in `schema.sql`) is the session uid.
mod common;

use serde_json::{json, Map, Value};
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

type Pool = sqlx::PgPool;

/// The template database + a session carrying `uid = 1` (owner of the
/// fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(common::test_config(), db);
    let mut data = ttrss::core::session::Session::new();
    data.set("uid", ttrss::core::phpser::Value::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
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

#[tokio::test]
async fn test_saveconfig_sets_prefs() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    let msg = ttrss::settings::saveconfig(
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_boolean_default() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // `boolean_prefs` lists a pref the form did not submit; it defaults to
    // "false" and is stored.
    let _ = ttrss::settings::saveconfig(
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_ignores_invalid_pref() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // A made-up pref name is not a valid pref key, so it is not stored.
    let _ = ttrss::settings::saveconfig(
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_blacklisted_tags_normalized() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // `BLACKLISTED_TAGS` is normalized: lowercased, numeric tags get the
    // `t:` prefix, de-duplicated, sorted ascending, joined with ", ".
    let _ = ttrss::settings::saveconfig(
        &state,
        &sess,
        &request(&[("BLACKLISTED_TAGS", json!("News, news, MAIN, 5, main"))]),
    )
    .await;

    assert_eq!(
        pref_value(db, "BLACKLISTED_TAGS").await.as_deref(),
        Some("main, news, t:5")
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_need_reload_on_theme_change() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // A changed `USER_CSS_THEME` (default "") triggers a reload.
    let msg = ttrss::settings::saveconfig(
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_no_reload_unchanged() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;

    // Saving an unchanged `USER_CSS_THEME` (still the default "") does not
    // trigger a reload.
    let msg =
        ttrss::settings::saveconfig(&state, &sess, &request(&[("USER_CSS_THEME", json!(""))]))
            .await;

    assert_eq!(msg, "The configuration was saved.");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_saveconfig_digest_time_resets() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // Seed a non-NULL digest time.
    sqlx::query("UPDATE ttrss_users SET last_digest_sent = '2024-01-01 00:00:00' WHERE id = 1")
        .execute(db)
        .await
        .unwrap();
    assert!(last_digest_sent(db).await.is_some());

    // A changed `DIGEST_PREFERRED_TIME` (default "00:00") clears it.
    let _ = ttrss::settings::saveconfig(
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
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_resetconfig_discards_overrides() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // Seed some overrides (plus `_PREFS_MIGRATED`, which `Prefs::reset` keeps).
    let _ = ttrss::settings::saveconfig(
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

    let msg = ttrss::settings::resetconfig(&state, &sess).await;
    assert_eq!(msg, "PREFS_NEED_RELOAD");

    // The overrides are gone; `_PREFS_MIGRATED` survives.
    assert_eq!(pref_value(db, "SHOW_CONTENT_PREVIEW").await, None);
    assert_eq!(pref_value(db, "USER_STYLESHEET").await, None);
    assert_eq!(
        pref_value(db, "_PREFS_MIGRATED").await.as_deref(),
        Some("1")
    );
        Ok(())
    })).await.unwrap();
}

/// Regression (the "Customize stylesheet" dialog): `RPC::setpref` must refresh
/// the per-process prefs cache on write, exactly like PHP `Prefs::_set` does via
/// `_set_cache`. Before the fix it persisted the pref (e.g. `USER_STYLESHEET`)
/// but left the cache holding the pre-save rows, so the next page render kept
/// serving the old shell until a process restart.
///
/// The cache is `pub(crate)` and `USER_STYLESHEET` has no public read path, so
/// this observes the same key-agnostic invalidation through a public cache
/// reader: `resolve_user_locale` resolves `USER_LANGUAGE` from the cached rows.
#[tokio::test]
async fn test_set_pref_refreshes_prefs_cache() {
    with_test_db(|url| Box::pin(async move {
        let db = common::make_db_from_url(&url).await.expect("test db");
        // Real `tt-rss` docroot so the i18n cache sees the shipped locales
        // (`resolve_user_locale` only resolves a non-English `USER_LANGUAGE`
        // when that locale is available).
        let mut cfg = common::test_config();
        cfg.docroot =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("tt-rss");
        let state = AppState::new(cfg, db);
        let mut data = ttrss::core::session::Session::new();
        data.set("uid", ttrss::core::phpser::Value::Int(1));
        let sess = Sess::new("test-sid".to_string());
        sess.replace("test-sid".to_string(), data);


        // Prime the cache with the current (no `USER_LANGUAGE`) rows → None.
        let before = ttrss::i18n::resolve_user_locale(&state, 1, None, None).await;
        assert_eq!(before, None, "no locale before the write");

        // Save `USER_LANGUAGE` through the same RPC the customize dialog uses.
        let _ = ttrss::settings::set_pref(
            &state,
            &sess,
            &request(&[("key", json!("USER_LANGUAGE")), ("value", json!("de_DE"))]),
        )
        .await;

        // The cache must now reflect the write (not the pre-save rows).
        let after = ttrss::i18n::resolve_user_locale(&state, 1, None, None).await;
        assert_eq!(
            after,
            Some("de_DE".to_string()),
            "USER_LANGUAGE should resolve from the (refreshed) cache after set_pref"
        );
        Ok(())
    })).await.unwrap();
}
