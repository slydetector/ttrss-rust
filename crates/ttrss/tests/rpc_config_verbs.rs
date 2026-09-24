//! DB tests for the `RPC` config verbs (M4): `setpref`, `togglepref`,
//! `setWidescreen`, and `log` (the client-JS error reporter).
//!
//! Rust-only: the frozen PHP suite has no tests for these small handlers, so
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

#[tokio::test]
async fn test_set_pref_sets_bool() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // `COMBINED_DISPLAY_MODE` default is "true"; posting "false" stores "" and
    // echoes the posted value back.
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[
            ("key", json!("COMBINED_DISPLAY_MODE")),
            ("value", json!("false")),
        ]),
    )
    .await;
    assert_eq!(
        pref_value(db, "COMBINED_DISPLAY_MODE").await.as_deref(),
        Some("")
    );
    assert_eq!(
        rv,
        json!({ "param": "COMBINED_DISPLAY_MODE", "value": "false" })
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_set_pref_stylesheet_keeps_tags() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // `USER_STYLESHEET`: `strip_tags` is off, so a CSS value with `<...>` is
    // stored verbatim (not mangled).
    let posted = "<div>body { }</div>";
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[("key", json!("USER_STYLESHEET")), ("value", json!(posted))]),
    )
    .await;
    assert_eq!(
        pref_value(db, "USER_STYLESHEET").await.as_deref(),
        Some(posted)
    );
    assert_eq!(rv, json!({ "param": "USER_STYLESHEET", "value": posted }));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_set_pref_strips_tags_other() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // A non-stylesheet Str pref: `strip_tags` is on, so the stored value is
    // stripped while the response echoes the *posted* value.
    let rv = ttrss::settings::set_pref(
        &state,
        &sess,
        &request(&[
            ("key", json!("SHORT_DATE_FORMAT")),
            ("value", json!("<b>M d</b>")),
        ]),
    )
    .await;
    assert_eq!(
        pref_value(db, "SHORT_DATE_FORMAT").await.as_deref(),
        Some("M d")
    );
    assert_eq!(
        rv,
        json!({ "param": "SHORT_DATE_FORMAT", "value": "<b>M d</b>" })
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_toggle_pref_flips_bool() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // `HIDE_READ_FEEDS` default is "false"; toggling stores "1" and echoes the
    // resolved typed value (a JSON bool for a T_BOOL pref).
    let rv1 = ttrss::settings::toggle_pref(
        &state,
        &sess,
        &request(&[("key", json!("HIDE_READ_FEEDS"))]),
    )
    .await;
    assert_eq!(
        pref_value(db, "HIDE_READ_FEEDS").await.as_deref(),
        Some("1")
    );
    assert_eq!(rv1, json!({ "param": "HIDE_READ_FEEDS", "value": true }));

    // Toggling again flips it back to false.
    let rv2 = ttrss::settings::toggle_pref(
        &state,
        &sess,
        &request(&[("key", json!("HIDE_READ_FEEDS"))]),
    )
    .await;
    assert_eq!(pref_value(db, "HIDE_READ_FEEDS").await.as_deref(), Some(""));
    assert_eq!(rv2, json!({ "param": "HIDE_READ_FEEDS", "value": false }));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_set_widescreen() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    let rv1 =
        ttrss::settings::set_widescreen(&state, &sess, &request(&[("wide", json!("1"))])).await;
    assert_eq!(
        pref_value(db, "WIDESCREEN_MODE").await.as_deref(),
        Some("1")
    );
    assert_eq!(rv1, json!({ "wide": 1 }));

    let rv2 =
        ttrss::settings::set_widescreen(&state, &sess, &request(&[("wide", json!("0"))])).await;
    assert_eq!(pref_value(db, "WIDESCREEN_MODE").await.as_deref(), Some(""));
    assert_eq!(rv2, json!({ "wide": 0 }));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_client_log_acks() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;

    let rv = ttrss::settings::client_log(
        &state,
        &sess,
        &request(&[
            ("msg", json!("some client error")),
            ("file", json!("js/foo.js")),
            ("line", json!("42")),
            ("context", json!("ctx")),
        ]),
    )
    .await;
    assert_eq!(rv, json!({ "message": "HOST_ERROR_LOGGED" }));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_client_log_empty_noop() {
    with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;

    // An empty `msg` logs nothing and echoes nothing (JSON `null`).
    let rv = ttrss::settings::client_log(&state, &sess, &request(&[("msg", json!(""))])).await;
    assert_eq!(rv, Value::Null);
        Ok(())
    })).await.unwrap();
}
