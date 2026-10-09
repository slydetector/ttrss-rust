//! Test: M7.13 — the `share` plugin (share an article by a unique URL; the key
//! is a short uuid in the **`ttrss_user_entries.uuid`** column, keyed
//! `ref_id` + `owner_uid`).
//!
//! The PHP has no unit tests for this plugin; these are new behavior coverage
//! for the M7.13 verify bar — the key lifecycle (`newkey` / `unshare` /
//! `clearArticleKeys`), the `shareDialog` (reuses a stored key, mints one when
//! empty, "Article not found." with no row), and the owner scoping (a different
//! owner cannot touch or read the article's key). Driven through
//! `pluginhost::dispatch::pluginhandler` (the protected path) exactly as the
//! RPC layer would; the no-account public route (`get`) is covered separately.

mod common;

use std::sync::Arc;
use test_db::template::with_test_db;

use serde_json::{Map, Value, json};
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::dispatch::{pluginhandler, public_dispatch};
use ttrss::pluginhost::plugin::MethodOutput;
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

/// A logged-in session for `uid` with CSRF token `tok` (as login would create).
fn session(uid: i32) -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(uid as i64));
    data.set("access_level", PValue::Int(100));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("share-test".into());
    sess.replace("share-test".into(), data);
    sess
}

/// The request map for a `share` plugin method (id-only methods).
fn req(id: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("plugin".into(), json!("share"));
    if let Some(i) = id {
        m.insert("id".into(), json!(i));
    }
    m
}

fn json_of(v: &MethodOutput) -> &Value {
    match v {
        MethodOutput::Json(j) => j,
        MethodOutput::Raw(s) => panic!("expected JSON, got raw: {s}"),
        MethodOutput::Response { .. } => panic!("expected JSON, got a response"),
    }
}

fn raw(v: &MethodOutput) -> &str {
    match v {
        MethodOutput::Raw(s) => s,
        MethodOutput::Response { body, .. } => body,
        MethodOutput::Json(j) => panic!("expected raw HTML, got JSON: {j}"),
    }
}

/// The article's stored uuid (the seed value or the plugin's key).
async fn stored_uuid(pool: &sqlx::PgPool, ref_id: i32, owner: i32) -> String {
    sqlx::query_scalar("SELECT uuid FROM ttrss_user_entries WHERE int_id = $1 AND owner_uid = $2")
        .bind(ref_id)
        .bind(owner)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or_default()
}

/// Count of the owner's articles that still carry a non-empty uuid.
async fn shared_count(pool: &sqlx::PgPool, owner: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM ttrss_user_entries WHERE owner_uid = $1 AND uuid <> ''",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap()
}

// `newkey` mints a fresh uuid for the article (overwriting the seed's key) and
// echoes `{"link": …}`.
#[tokio::test]
async fn test_newkey_mints_key() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);
            let before = stored_uuid(state.db.pool(), 1, 1).await;

            let v = pluginhandler(&state, &sess, "tok", "newkey", &req(Some("1")))
                .await
                .unwrap();
            let j = json_of(&v);
            let new_key: String = j["link"].as_str().unwrap().into();
            assert!(!new_key.is_empty());
            assert_ne!(new_key, before, "newkey should overwrite the seed key");

            assert_eq!(stored_uuid(state.db.pool(), 1, 1).await, new_key);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `unshare` clears the article's uuid + echoes the message.
#[tokio::test]
async fn test_unshare_clears_key() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "unshare", &req(Some("1")))
                .await
                .unwrap();
            assert_eq!(raw(&v), "Article unshared");
            assert_eq!(stored_uuid(state.db.pool(), 1, 1).await, "");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `clearArticleKeys` blanks every one of the owner's article keys.
#[tokio::test]
async fn test_clear_article_keys() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);
            assert!(
                shared_count(state.db.pool(), 1).await > 0,
                "seed should pre-share"
            );

            let v = pluginhandler(&state, &sess, "tok", "cleararticlekeys", &req(None))
                .await
                .unwrap();
            assert_eq!(raw(&v), "Shared URLs cleared.");
            assert_eq!(shared_count(state.db.pool(), 1).await, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `shareDialog` reuses a stored key (the seed's `uuid-hn-001` for article 1)
// without minting a new one.
#[tokio::test]
async fn test_share_dialog_reuses_key() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("1")))
                .await
                .unwrap();
            let html = raw(&v);
            // The seed key is used in the share URL…
            assert!(html.contains("key=uuid-hn-001"), "{html}");
            // …and the footer buttons are present.
            assert!(html.contains("Unshare article"), "{html}");
            assert!(html.contains("Generate new URL"), "{html}");
            assert!(html.contains("Close this window"), "{html}");
            // Reusing does not change the stored key.
            assert_eq!(stored_uuid(state.db.pool(), 1, 1).await, "uuid-hn-001");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `shareDialog` on an article whose key was cleared mints a fresh one.
#[tokio::test]
async fn test_share_dialog_mints_when_empty() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);

            // Clear the seed key first.
            pluginhandler(&state, &sess, "tok", "unshare", &req(Some("1")))
                .await
                .unwrap();
            assert_eq!(stored_uuid(state.db.pool(), 1, 1).await, "");

            let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("1")))
                .await
                .unwrap();
            let html = raw(&v);
            // A fresh key was minted (not the seed, not empty).
            let new_key = stored_uuid(state.db.pool(), 1, 1).await;
            assert!(!new_key.is_empty() && new_key != "uuid-hn-001");
            assert!(html.contains(&format!("key={new_key}")), "{html}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `shareDialog` for an article with no `ttrss_user_entries` row (owner 1 has no
// article 9999) → "Article not found.", no key minted.
#[tokio::test]
async fn test_share_dialog_no_row() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let sess = session(1);

            let v = pluginhandler(&state, &sess, "tok", "sharedialog", &req(Some("9999")))
                .await
                .unwrap();
            let html = raw(&v);
            assert!(html.contains("Article not found."), "{html}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// Owner scoping: owner 2 has no article 1, so neither its `unshare` clears
// owner 1's key nor its `shareDialog` can read it.
#[tokio::test]
async fn test_owner_scoping() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;
            let owner2 = session(2);

            // owner 2's unshare on owner 1's article is a no-op (no row for owner 2).
            pluginhandler(&state, &owner2, "tok", "unshare", &req(Some("1")))
                .await
                .unwrap();
            assert_eq!(
                stored_uuid(state.db.pool(), 1, 1).await,
                "uuid-hn-001",
                "owner 2 must not clear owner 1's key"
            );

            // …and owner 2's dialog cannot read owner 1's article.
            let v = pluginhandler(&state, &owner2, "tok", "sharedialog", &req(Some("1")))
                .await
                .unwrap();
            assert!(raw(&v).contains("Article not found."), "{}", raw(&v));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `get` public route: a valid key serves the article page (200, text/html).
#[tokio::test]
async fn test_get_public_route() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;

            // The seed pre-sets uuid-hn-001 on article 1 (owner 1).
            let v = public_dispatch(
                &state,
                "share",
                "get",
                &json!({"key": "uuid-hn-001"}),
                &state.config.self_url_rtrim(),
            )
            .await
            .unwrap();
            let MethodOutput::Response {
                status,
                content_type,
                body,
            } = v
            else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 200, "body: {body}");
            assert!(
                content_type.starts_with("text/html"),
                "content_type: {content_type}"
            );
            assert!(body.contains("<!DOCTYPE html>"), "body: {body}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// `get` public route: an unknown key → 404 "Article not found."
#[tokio::test]
async fn test_get_public_route_404() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url, "share").await;

            let v = public_dispatch(
                &state,
                "share",
                "get",
                &json!({"key": "nonexistent"}),
                &state.config.self_url_rtrim(),
            )
            .await
            .unwrap();
            let MethodOutput::Response { status, body, .. } = v else {
                panic!("expected a Response, got {v:?}");
            };
            assert_eq!(status, 404, "body: {body}");
            assert_eq!(body, "Article not found.");
            Ok(())
        })
    })
    .await
    .unwrap();
}

// M7.22 item 3 — the public `get` route runs `HOOK_RENDER_ARTICLE` (PHP
// `share/init.php:149`) against the GLOBAL host (`getInstance()`): on an
// unauthenticated public request that is the SYSTEM plugin set
// (`init_plugins`, `functions.php:424`). The dispatch ctx models that as
// system ∪ {named}. Two faithful pins:
//
// - the route's host actually iterates nsfw for `HOOK_RENDER_ARTICLE`
//   (the seam — before the fix the host held only `share`);
// - the page is unwrapped: plugin storage is owner-scoped and the route is
//   owner-0 (the unwritable/read-less system scope), so the nsfw tag list
//   is empty — exactly the reference's anonymous-page behavior.
#[tokio::test]
async fn test_get_public_route_render_chain_seam() {
    with_test_db(|url| Box::pin(async move {
    let state = setup(&url, "share,nsfw").await;
    {
        let mut c = ttrss::pluginhost::host::HostCtx::for_user(
            Arc::clone(&state.plugins),
            state.db.pool().clone(),
            state.config.clone(),
            Some(1),
            None,
            0,
            &["share", "nsfw"],
            &[],
        );
        let host = c.host_arc();

        let plugin = host.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
            .await
            .unwrap();
        // Tag article 1 (its user entry has int_id 1 in the seed).
        sqlx::query("INSERT INTO ttrss_tags (tag_name, owner_uid, post_int_id) VALUES ('nsfw', 1, 1)")
            .execute(state.db.pool())
            .await
            .unwrap();
    }

    // The seam: the route's host (system ∪ {share}, owner 0) iterates nsfw
    // for the render hook — the pre-fix single-plugin host iterated none.
    let system: Vec<&str> = state
        .config
        .plugins
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let route_ctx = ttrss::pluginhost::host::HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(0),
        None,
        0,
        &system,
        &["share"],
    );
    let old_model = ttrss::pluginhost::host::HostCtx::single(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        "share",
    );
    assert!(
        old_model.hook_order("hook_render_article").is_empty(),
        "single-plugin host has no render-article plugins (the pre-fix model)"
    );
    assert!(
        !route_ctx.hook_order("hook_render_article").is_empty(),
        "route host (system + named) must iterate the render hook"
    );

    // The page: owner-0 storage is empty → no tags match → no toggle.
    let v = public_dispatch(
        &state,
        "share",
        "get",
        &json!({"key": "uuid-hn-001"}),
        &state.config.self_url_rtrim(),
    )
    .await
    .unwrap();
    let MethodOutput::Response { status, body, .. } = v else {
        panic!("expected a Response, got {v:?}");
    };
    assert_eq!(status, 200, "body: {body}");
    assert!(
        !body.contains("<details class='nsfw'>"),
        "anonymous owner-0 route must NOT see owner-1 nsfw tags; body: {body}"
    );
        Ok(())
    })).await.unwrap();
}
