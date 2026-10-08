//! Test parity: M7.6 — the `no_iframes` plugin (iframes removed during
//! `HOOK_SANITIZE` unless `HOOK_IFRAME_WHITELISTED` says otherwise).
//!
//! The PHP `no_iframes` plugin has no dedicated suite in the frozen reference
//! (the M7 "no per-plugin PHP suites" note); the cases here pin the M7.6
//! verify bar: the full async `sanitize` pipeline (core + the `HOOK_SANITIZE`
//! stage) removes iframes when the plugin is active and is a no-op when it
//! is not, including the no-`src` iframe (PHP: no host → not whitelisted).
//! The whitelist chain itself is pinned by the unit tests (a custom
//! whitelisting plugin cannot live in `AppState`'s fixed registry; M7.11's
//! `af_youtube_embed` is the real consumer).

mod common;

use std::sync::Arc;
use test_db::template::with_test_db;

use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::HostCtx;
use ttrss::sanitizer;
use ttrss::session::Sess;

fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_pass: "postgres".into(),
        db_sslmode: "disable".into(),
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        plugins: "no_iframes".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        cache_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        http_429_throttle_interval: 3600,
        daemon_sleep_interval: 60,
        schedule_log_daemon_update_login_limit_users: String::from("@hourly"),
        ..Default::default()
    }
}

async fn setup(url: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(test_config(), db)
}

fn session() -> Sess {
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(100));
    data.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let sess = Sess::new("no-iframe-test".into());
    sess.replace("no-iframe-test".into(), data);
    sess
}

/// The full async `sanitize` (core + the M7.6 `HOOK_SANITIZE` hook stage) with the
/// given active sets — the full pipeline the read paths use.
///
/// `has_sandbox` is `true`: with a sandbox-capable client the CORE keeps
/// (and sandboxes) iframes — `iframe` only enters the allowed-elements set
/// then — so removing them is exclusively the plugin's job (with
/// `has_sandbox = false` the core strips them and the plugin is moot).
async fn pipeline(state: &AppState, system: &[&str], user: &[&str], input: &str) -> String {
    let mut c = HostCtx::for_user(
        Arc::clone(&state.plugins),
        state.db.pool().clone(),
        state.config.clone(),
        Some(1),
        None,
        0,
        system,
        user,
    );
    sanitizer::sanitize(
        input,
        false,
        false,
        true,
        false,
        Some("http://site.example/"),
        None,
        42,
        Some(&mut c),
    )
    .await
}

#[tokio::test]
async fn test_pipeline_removes_iframes() {
    with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    let _sess = session();

    // no_iframes active (system list).
    let out = pipeline(
        &state,
        &["no_iframes"],
        &[],
        "<p>a</p><iframe src=\"https://evil.example/x\" width=\"100\"></iframe><p>b</p><iframe></iframe><p>c</p>",
    )
    .await;
    // Both iframes go: the one with a src (host not whitelisted — no
    // whitelisting plugin registered) and the one without a src (PHP: no
    // host → not whitelisted). Everything else survives the core pass.
    assert!(!out.contains("<iframe"), "iframes survived: {out}");
    assert!(out.contains("a"), "{out}");
    assert!(out.contains("b"), "{out}");
    assert!(out.contains("c"), "{out}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_pipeline_noop_when_inactive() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url).await;
            let _sess = session();

            // Registered (it is in the registry) but not active for this owner:
            // the hook stage has an empty order and the iframe survives the core
            // pass — sandboxed, as `process_iframes` leaves it.
            let out = pipeline(
                &state,
                &[],
                &[],
                "<p>a</p><iframe src=\"https://evil.example/x\"></iframe><p>b</p>",
            )
            .await;
            assert!(
                out.contains("<iframe"),
                "iframe was removed without the plugin: {out}"
            );
            assert!(out.contains("allow-scripts"), "core sandbox missing: {out}");
            assert!(out.contains("a"), "{out}");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_enabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = setup(&url).await;
            let _sess = session();

            // no_iframes is not a system plugin: the user-enabled list activates it
            // (the `_ENABLED_PLUGINS` shape the dispatch ctxs use).
            let out = pipeline(
                &state,
                &[],
                &["no_iframes"],
                "<p>a</p><iframe src=\"https://evil.example/x\"></iframe>",
            )
            .await;
            assert!(!out.contains("<iframe"), "iframes survived: {out}");
            Ok(())
        })
    })
    .await
    .unwrap();
}
