//! Test parity: M7.5 — the `nsfw` plugin (article content hidden behind a
//! `<details>` toggle based on a per-owner tag list in plugin storage).
//!
//! The PHP `nsfw` plugin has no dedicated test suite in the frozen reference
//! (the M7 "no per-plugin PHP suites" note); the cases here pin the M7.5
//! verify bar: the `save` round-trip (normalization + storage), the
//! `rewrite_contents` wrapping (render hooks), the `headline`/`article`
//! fallback of the API hook, the placeholder image, the prefs-tab pane, and
//! the plugin-level CSRF gate.

mod common;

use test_db::template::with_test_db;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::dispatch::pluginhandler;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::MethodOutput;
use ttrss::session::Sess;
use ttrss::AppState;

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
        auth_auto_create: true,
        auth_remote_post_logout_url: String::new(),
        plugins: "nsfw".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        docroot: std::path::PathBuf::from("tt-rss"),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        max_cache_file_size: 64 * 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_max_jobs: 2,
        daemon_unsuccessful_days_limit: 30,
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_expire_lock_files: String::from("@hourly"),
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@hourly"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
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
    let sess = Sess::new("nsfw-test".into());
    sess.replace("nsfw-test".into(), data);
    sess
}

fn params(pairs: &[(&str, &str)]) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert((*k).into(), json!(v));
    }
    m
}

fn raw(v: &MethodOutput) -> &str {
    match v {
        MethodOutput::Raw(s) => s,
        MethodOutput::Response { body, .. } => body,
        MethodOutput::Json(j) => panic!("expected raw output, got JSON: {j}"),
    }
}

fn code(v: &MethodOutput) -> &str {
    match v {
        MethodOutput::Json(j) => j["error"]["code"].as_str().unwrap(),
        MethodOutput::Raw(s) => panic!("expected JSON error, got raw output: {s}"),
        MethodOutput::Response { .. } => panic!("expected JSON error, got a response"),
    }
}

/// A dispatch-equivalent context (nsfw active for owner 1).
fn ctx(state: &AppState) -> HostCtx<'_> {
    HostCtx::for_user(&state.plugins, state.db.pool(), &state.config, Some(1), None, 0, &[], &["nsfw"])
}

/// The stored `tags` value (raw plugin-storage read).
async fn stored_tags(ctx: &mut HostCtx<'_>) -> String {
    let plugin = ctx.get_plugin("nsfw").expect("nsfw active");
    ctx.storage_get(plugin, "tags", PValue::Null)
        .await
        .unwrap()
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn test_save_roundtrip() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    let sess = session();

    let v = pluginhandler(
        &state,
        &sess,
        "tok",
        "save",
        &params(&[("plugin", "nsfw"), ("tags", "  a, B ,a ")]),
    )
    .await
    .unwrap();
    assert_eq!(raw(&v), "Configuration saved.");

    // `save` normalizes (trim, lowercase, de-dup, sort) and stores.
    let mut c = ctx(&state);
    assert_eq!(stored_tags(&mut c).await, "a, b");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_render_wrap() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;

    // Seed the stored tag list directly (as `save` would).
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw, adult".to_vec()))
            .await
            .unwrap();
    }

    let plugin = state.plugins.get_plugin("nsfw").expect("nsfw registered");
    let mut c = ctx(&state);

    // Matching article: the content is wrapped.
    let mut article = json!({ "content": "<p>body</p>", "tags": ["nsfw", "other"] });
    let out = plugin.hook_render_article(&mut c, &mut article).await;
    assert_eq!(
        out["content"],
        "<details class='nsfw'><summary>Not safe for work (click to toggle)</summary><p>body</p></details>"
    );

    // Non-matching article: unchanged.
    let mut article2 = json!({ "content": "<p>body</p>", "tags": ["other"] });
    let out = plugin.hook_render_article_cdm(&mut c, &mut article2).await;
    assert_eq!(out["content"], "<p>body</p>");

    // No `tags` key at all: unchanged.
    let mut article3 = json!({ "content": "<p>x</p>" });
    let out = plugin.hook_render_article(&mut c, &mut article3).await;
    assert_eq!(out["content"], "<p>x</p>");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_render_api_fallback() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
            .await
            .unwrap();
    }

    let plugin = state.plugins.get_plugin("nsfw").expect("nsfw registered");
    let mut c = ctx(&state);

    // `$row['headline'] ?? $row['article']` — the headline wins when present…
    let row = json!({
        "headline": { "content": "<h>hl</h>", "tags": ["nsfw"] },
        "article": { "content": "<a>art</a>", "tags": ["nsfw"] },
    });
    let out = plugin.hook_render_article_api(&mut c, &row).await;
    assert_eq!(
        out["content"],
        "<details class='nsfw'><summary>Not safe for work (click to toggle)</summary><h>hl</h></details>"
    );

    // …and the article when the headline key is absent (the *article* is
    // what comes back, as in PHP).
    let row2 = json!({ "article": { "content": "<a>art</a>", "tags": ["nsfw"] } });
    let out = plugin.hook_render_article_api(&mut c, &row2).await;
    assert_eq!(
        out["content"],
        "<details class='nsfw'><summary>Not safe for work (click to toggle)</summary><a>art</a></details>"
    );
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_image_hook() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
            .await
            .unwrap();
    }

    let plugin = state.plugins.get_plugin("nsfw").expect("nsfw registered");
    // PHP: `Config::get_self_url() . "/plugins/nsfw/nsfw.png"` — the base carries
    // no trailing slash.
    let mut c = ctx(&state).with_self_url("http://127.0.0.1:8280/tt-rss");

    let matching = json!({ "tags": ["nsfw"] });
    let v = plugin
        .hook_article_image(&mut c, &json!([]), "<img src='real.png'>", "http://site/", &matching)
        .await;
    assert_eq!(
        v,
        json!(["http://127.0.0.1:8280/tt-rss/plugins/nsfw/nsfw.png", "", "nsfw"])
    );

    let non_matching = json!({ "tags": ["other"] });
    let v = plugin
        .hook_article_image(&mut c, &json!([]), "<img src='real.png'>", "http://site/", &non_matching)
        .await;
    assert_eq!(v, json!(["", "", "<img src='real.png'>"]));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_prefs_tab() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"a, b".to_vec()))
            .await
            .unwrap();
    }

    let plugin = state.plugins.get_plugin("nsfw").expect("nsfw registered");
    let mut c = ctx(&state);

    // A different tab name prints nothing.
    plugin.hook_prefs_tab(&mut c, "otherTab").await;
    assert_eq!(c.output, "");

    plugin.hook_prefs_tab(&mut c, "prefPrefs").await;
    let html = &c.output;
    // The pane.
    assert!(html.contains("dojoType=\"dijit.layout.AccordionPane\""), "{html}");
    assert!(html.contains(">extension</i> NSFW Plugin"), "{html}");
    // The form the pane posts to (the `save` method, plugin `nsfw`).
    assert!(html.contains("name=\"plugin\""), "{html}");
    assert!(html.contains("value=\"nsfw\""), "{html}");
    assert!(html.contains("name=\"method\""), "{html}");
    assert!(html.contains("value=\"save\""), "{html}");
    // The caption + the stored tags in the textarea.
    assert!(html.contains("Tags to consider NSFW (comma-separated):"), "{html}");
    assert!(html.contains("name='tags'>a, b</textarea>"), "{html}");
    // The Save button.
    assert!(html.contains(">Save</button>"), "{html}");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_csrf_gate() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    let sess = session();

    // `save` is not `csrf_ignore`d.
    let v = pluginhandler(
        &state,
        &sess,
        "WRONG",
        "save",
        &params(&[("plugin", "nsfw"), ("tags", "x")]),
    )
    .await
    .unwrap();
    assert_eq!(code(&v), "E_UNAUTHORIZED");
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_disabled_plugin_unknown() {
    let _ = with_test_db(|url| Box::pin(async move {
    // A registered but system-disabled plugin is an unknown plugin on the
    // protected path (M7.3's active-aware `get_plugin`).
    let state = setup(&url).await;
    let sess = session();
    let v = pluginhandler(
        &state,
        &sess,
        "tok",
        "save",
        &params(&[("plugin", "note"), ("tags", "x")]),
    )
    .await
    .unwrap();
    assert_eq!(code(&v), "E_UNKNOWN_PLUGIN");
        Ok(())
    })).await.unwrap();
}

// M7.22 item 4 — HOOK_ARTICLE_IMAGE runs inside `article::get_image` (PHP
// `Article.php:571`): a matching article gets the placeholder as its
// representative image; a non-matching one falls through to the DOM parse.
#[tokio::test]
async fn test_get_image_runs_article_image_chain() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
            .await
            .unwrap();
    }
    let content = "<img src='https://example.com/real.png'>";
    let matching = json!({ "id": 1, "tags": ["nsfw"] });
    let mut c = ctx(&state).with_self_url("http://127.0.0.1:8280/tt-rss");
    let (image, stream, kind) =
        ttrss::article::get_image(&mut c, &[], content, "https://example.com", &matching).await;
    assert_eq!(
        image,
        "http://127.0.0.1:8280/tt-rss/plugins/nsfw/nsfw.png",
        "chain should short-circuit the DOM parse"
    );
    assert_eq!(stream, "");
    assert_eq!(kind, 0, "plugin-provided image keeps kind 0 (PHP)");

    let non_matching = json!({ "id": 1, "tags": ["other"] });
    let (image, _stream, _kind) =
        ttrss::article::get_image(&mut c, &[], content, "https://example.com", &non_matching).await;
    assert!(image.contains("example.com/real.png"), "image was: {image}");
        Ok(())
    })).await.unwrap();
}

// M7.22 item 3 (seam) — the `HOOK_RENDER_ARTICLE` family runs through the
// `Feeds::view` (CDM) chain: an nsfw-tagged article's content is wrapped in
// the `<details>` toggle in the getHeadlines response. This also pins the
// missing-fold fix (M7.16): before it, the plugin's rewritten article was
// discarded by the chain runner.
#[tokio::test]
async fn test_cdm_view_runs_render_article_chain() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url).await;
    {
        let mut c = ctx(&state);
        let plugin = c.get_plugin("nsfw").expect("nsfw active");
        c.storage_set(plugin, "tags", PValue::Bytes(b"nsfw".to_vec()))
            .await
            .unwrap();
        // Tag article 1 (owner 1; its user entry has int_id 1 in the seed).
        // The tag_cache shadows ttrss_tags in `_get_tags` (PHP parity), so
        // the cache is updated too.
        sqlx::query("INSERT INTO ttrss_tags (tag_name, owner_uid, post_int_id) VALUES ('nsfw', 1, 1)")
            .execute(state.db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE ttrss_user_entries SET tag_cache = 'rust,nsfw' WHERE ref_id = 1 AND owner_uid = 1")
            .execute(state.db.pool())
            .await
            .unwrap();
    }

    fn sess() -> Sess {
        let mut d = Session::new();
        d.set("uid", PValue::Int(1));
        d.set("access_level", PValue::Int(100));
        d.set("pwd_hash", PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()));
        d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
        d.set("name", PValue::Bytes(b"admin".to_vec()));
        d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
        let s = Sess::new("nsfw-view".into());
        s.replace("nsfw-view".into(), d);
        s
    }
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/backend.php")
        .header(axum::http::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(
            "op=Feeds&method=view&feed=1&csrf_token=tok".to_string(),
        ))
        .unwrap();
    let res = ttrss::rpc::handler(
        axum::extract::State(state.clone()),
        axum::extract::Extension(sess()),
        req,
    )
    .await;
    let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let content = v
        .pointer("/headlines/content/0/content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    assert!(
        content.contains("<details class='nsfw'>"),
        "CDM content should carry the nsfw toggle: {content}"
    );
        Ok(())
    })).await.unwrap();
}
