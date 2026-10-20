//! Test parity: M7.4 — the `note` plugin (article notes in the
//! `ttrss_user_entries.note` column, keyed `ref_id` + `owner_uid`).
//!
//! The PHP `note` plugin has no dedicated test suite in the frozen reference
//! (the M7 "no per-plugin PHP suites" note); the cases here pin the M7.4
//! verify bar: save/load, per-article + owner scoping, the footer-only
//! no-row form of `edit`, the button-row markup, the CSRF gate, and the
//! active-set gating (a system-disabled plugin is an unknown plugin).
//!
//! Driven through `pluginhost::dispatch::pluginhandler` exactly as the RPC
//! layer would (a `Sess` with `uid` + `csrf_token`), plus one direct
//! `hook_article_button` call for the button row.

use crate::common;

use std::sync::Arc;
use test_db::template::with_test_db;

use serde_json::{Map, Value, json};
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::dispatch::pluginhandler;
use ttrss::pluginhost::plugin::MethodOutput;
use ttrss::pluginhost::{HostCtx, PluginHost};
use ttrss::session::Sess;

fn test_config(plugins: &str) -> Config {
    let mut c = common::test_config();
    c.plugins = plugins.into();
    c
}

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

/// A logged-in session for `uid` with CSRF token `tok` (as login would
/// create).
fn session(uid: i32) -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(uid as i64));
    data.set(
        "access_level",
        PValue::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64),
    );
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("note-test".into());
    sess.replace("note-test".into(), data);
    sess
}

fn req(id: &str, note: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("plugin".into(), json!("note"));
    m.insert("id".into(), json!(id));
    if let Some(n) = note {
        m.insert("note".into(), json!(n));
    }
    m
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap()
}

fn raw(v: &MethodOutput) -> &str {
    match v {
        MethodOutput::Raw(s) => s,
        MethodOutput::Response { body, .. } => body,
        MethodOutput::Json(j) => panic!("expected raw HTML, got JSON: {j}"),
    }
}

fn json_of(v: &MethodOutput) -> &Value {
    match v {
        MethodOutput::Json(j) => j,
        MethodOutput::Raw(s) => panic!("expected JSON, got raw HTML: {s}"),
        MethodOutput::Response { .. } => panic!("expected JSON, got a response"),
    }
}

// Save + load: `setnote` writes the note (tags stripped, as `clean()`), and
// `edit` returns the dialog body with the note in the textarea.
#[tokio::test]
async fn test_setnote_then_edit() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(
                &state,
                &sess,
                "tok",
                "setnote",
                &req("1", Some("hello note")),
            )
            .await
            .unwrap();
            let j = json_of(&v);
            assert_eq!(j["id"], 1);
            assert_eq!(j["note"], "hello note");

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("1", None))
                .await
                .unwrap();
            let html = raw(&v);
            // The form the dialog posts back to (hidden op/plugin/method + id).
            assert!(html.contains("name=\"op\""), "{html}");
            assert!(html.contains("value=\"PluginHandler\""), "{html}");
            assert!(html.contains("name=\"plugin\""), "{html}");
            assert!(html.contains("value=\"note\""), "{html}");
            assert!(html.contains("name=\"method\""), "{html}");
            assert!(html.contains("value=\"setnote\""), "{html}");
            assert!(html.contains("name=\"id\""), "{html}");
            assert!(html.contains("value=\"1\""), "{html}");
            // The note, in the textarea.
            assert!(html.contains("name='note'>hello note</textarea>"), "{html}");
            // The footer.
            assert!(html.contains("<footer class='text-center'>"), "{html}");
            assert!(html.contains(">Save</button>"), "{html}");
            assert!(html.contains(">Cancel</button>"), "{html}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `clean()` strips tags from the stored note (PHP's `clean($_REQUEST["note"])`).
#[tokio::test]
async fn test_setnote_strips_tags() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "setnote", &req("1", Some("a<b>c")))
                .await
                .unwrap();
            assert_eq!(json_of(&v)["note"], "ac");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `edit` for an article without a `ttrss_user_entries` row: footer only, no
// textarea (the row appears when the article is first read).
#[tokio::test]
async fn test_edit_no_row_footer_only() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("9999", None))
                .await
                .unwrap();
            let html = raw(&v);
            assert!(!html.contains("<textarea"), "{html}");
            assert!(html.contains("<footer class='text-center'>"), "{html}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `setnote` for an article without a row is a no-op (PHP's `UPDATE` without
// `INSERT`) — a later `edit` still sees no textarea.
#[tokio::test]
async fn test_setnote_no_row_noop() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "setnote", &req("9999", Some("x")))
                .await
                .unwrap();
            assert_eq!(json_of(&v)["id"], 9999);

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("9999", None))
                .await
                .unwrap();
            assert!(!raw(&v).contains("<textarea"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// Owner scoping: a note is per (article, owner). Owner 1's `setnote`/`edit`
// cannot see or touch owner 2's row.
#[tokio::test]
async fn test_owner_scoping() {
    with_test_db(|url| Box::pin(async move {
    let state = setup(&url, "note").await;

    // A self-contained fixture: owner 2 + an entry no one else reads (the
    // seed gives owner 1 a row for every seeded article).
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO ttrss_users (login, pwd_hash, access_level) VALUES ('owner2', 'x', {})", ttrss::core::user::ACCESS_LEVEL_ADMIN)))
        .execute(state.db.pool())
        .await
        .unwrap();
    let ref_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_entries          (title, guid, link, updated, content, content_hash, date_entered, date_updated)          VALUES ('o2 only', 'uuid-o2-only', 'https://example.com/o2', now(), '', '', now(), now())          RETURNING id",
    )
    .fetch_one(state.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_user_entries          (ref_id, uuid, owner_uid, tag_cache, label_cache)          VALUES ($1, 'uuid-o2-entry', 2, '', '')",
    )
    .bind(ref_id)
    .execute(state.db.pool())
    .await
    .unwrap();

    let sess1 = session(1);
    let mut data = Session::new();
    data.set("uid", PValue::Int(2));
    data.set("access_level", PValue::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess2 = Sess::new("note-test-2".into());
    sess2.replace("note-test-2".into(), data);


    // Owner 1 has no row for the entry: `edit` is footer-only…
    let v = pluginhandler(&state, &sess1, "tok", "edit", &req(&ref_id.to_string(), None)).await.unwrap();
    assert!(!raw(&v).contains("<textarea"));
    // …and owner 1's `setnote` is a no-op (owner 2's row is untouched)…
    let v = pluginhandler(&state, &sess1, "tok", "setnote", &req(&ref_id.to_string(), Some("sneaky"))).await.unwrap();
    assert_eq!(json_of(&v)["id"], ref_id);

    // …while owner 2 sets and reads their own note.
    let v = pluginhandler(&state, &sess2, "tok", "setnote", &req(&ref_id.to_string(), Some("o2 note"))).await.unwrap();
    assert_eq!(json_of(&v)["note"], "o2 note");
    let v = pluginhandler(&state, &sess2, "tok", "edit", &req(&ref_id.to_string(), None)).await.unwrap();
    assert!(raw(&v).contains("name='note'>o2 note</textarea>"));
    // Owner 1 still sees no row of their own.
    let v = pluginhandler(&state, &sess1, "tok", "edit", &req(&ref_id.to_string(), None)).await.unwrap();
    assert!(!raw(&v).contains("<textarea"));
        Ok(())
    })).await.unwrap();
}

// Per-article scoping: a note on entry 1 does not leak into entry 2's dialog
// (entry 2's row exists with an empty note → the textarea renders empty).
#[tokio::test]
async fn test_per_article_scoping() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(
                &state,
                &sess,
                "tok",
                "setnote",
                &req("1", Some("entry one")),
            )
            .await
            .unwrap();
            assert_eq!(json_of(&v)["note"], "entry one");

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("2", None))
                .await
                .unwrap();
            let html = raw(&v);
            assert!(html.contains("<textarea"), "{html}"); // row exists (note empty)
            assert!(!html.contains("entry one"), "{html}");

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("1", None))
                .await
                .unwrap();
            assert!(raw(&v).contains("name='note'>entry one</textarea>"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The plugin-level CSRF gate: `note` does not `csrf_ignore`, so a bad token
// is rejected even for an existing method.
#[tokio::test]
async fn test_csrf_fail() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "WRONG", "setnote", &req("1", Some("x")))
                .await
                .unwrap();
            assert_eq!(code(json_of(&v)), "E_UNAUTHORIZED");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// Active-set gating: a registered but system-disabled plugin is an unknown
// plugin (PHP's per-request host only loads the enabled set).
#[tokio::test]
async fn test_inactive_plugin_unknown() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "auth_internal").await; // `note` not enabled
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "edit", &req("1", None))
                .await
                .unwrap();
            assert_eq!(code(json_of(&v)), "E_UNKNOWN_PLUGIN");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The button row (`hook_article_button`): the exact markup the article list
// gets from the hook (attribute order + visible text as PHP's literal).
#[tokio::test]
async fn test_button_row() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let plugin = state.plugins.get_plugin("note").expect("note registered");
            let mut ctx = HostCtx::empty(
                Arc::clone(&state.plugins),
                state.db.pool().clone(),
                state.config.clone(),
            );

            let row = plugin
                .hook_article_button(&mut ctx, &json!({ "id": 42 }))
                .await;
            assert_eq!(
                row,
                "<i class='material-icons' onclick=\"Plugins.Note.edit(42)\" \
         style='cursor : pointer' title=\"Edit article note\">note</i>"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `method_exists` parity: case-insensitive, exactly the two methods.
#[tokio::test]
async fn test_method_exists() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "note").await;
            let plugin = state.plugins.get_plugin("note").expect("note registered");

            for m in ["edit", "setnote", "EDIT", "SetNote"] {
                assert!(plugin.has_public_method(m), "{m}");
            }
            for m in ["set", "getnote", "clear", ""] {
                assert!(!plugin.has_public_method(m), "{m}");
            }

            // The registry exposes the plugin under both the dir name and resolves
            // case-insensitively (PHP `get_plugin` lowercases).
            let _ = state.plugins.get_plugin("NOTE").expect("case-insensitive");
            let _ = state.plugins.get_plugin("note");
            let _ = state.plugins.get_plugin("nonexistent");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// The registry `about()` metadata (the plugin list surface, M4).
#[test]
fn about_metadata() {
    let host = PluginHost::startup(&ttrss::pluginhost::all_plugins());
    let p = host.get_plugin("note").expect("note registered");
    let about = p.about();
    assert_eq!(about.description, "Adds support for setting article notes");
    assert_eq!(about.author, "fox");
    assert!(!about.system);
    assert_eq!(p.api_version(), 2);
}
