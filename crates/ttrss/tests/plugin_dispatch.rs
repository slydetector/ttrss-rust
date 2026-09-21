//! Test parity: M7.3 — the plugin method dispatch endpoints.
//!
//! PHP has no dedicated suite for `PluginHandler`/`pluginhandler` (the
//! mechanism is exercised by the individual plugin suites); the cases here
//! pin the M7.3 verify bar: dispatch / unknown plugin / unknown method /
//! CSRF fail / `csrf_ignore` bypass / `is_public_method` gating.
//!
//! The protected path is driven through `pluginhost::dispatch::pluginhandler`
//! with a `Sess` carrying `uid`/`csrf_token` (as `rpc::dispatch` would); the
//! public path through `pluginhost::dispatch::public_dispatch` (no session).

mod common;

use std::sync::Arc;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::pluginhost::host::{HostCtx, PluginHost};
use ttrss::pluginhost::plugin::{About, BoxFuture, MethodOutput, Plugin, PluginError};
use ttrss::session::Sess;
use ttrss::AppState;

// ── the test double plugin ──────────────────────────────────────────────────

/// A plugin whose dispatch surface is configurable: which methods exist
/// (`has_public_method`), which the public endpoint may call
/// (`is_public_method`), which skip the plugin-level CSRF check.
struct DispatchPlugin {
    public: bool,
    csrf_ignore: bool,
    methods: Vec<&'static str>,
}

impl Plugin for DispatchPlugin {
    fn name(&self) -> &'static str {
        "dispatchtest"
    }
    fn class_name(&self) -> &'static str {
        "DispatchTest"
    }
    fn about(&self) -> About {
        About::new("dispatch test double", "tester")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, _host: &mut PluginHost) {}
    fn is_public_method(&self, method: &str) -> bool {
        self.public && self.methods.contains(&method)
    }
    fn csrf_ignore(&self, method: &str) -> bool {
        self.csrf_ignore && self.methods.contains(&method)
    }
    fn has_public_method(&self, method: &str) -> bool {
        self.methods.contains(&method)
    }
    fn invoke_method<'a, 'h: 'a>(
        &'a self,
        ctx: &'a mut HostCtx<'h>,
        method: &'a str,
        params: &'a Value,
    ) -> BoxFuture<'a, Result<MethodOutput, PluginError>> {
        Box::pin(async move {
            if !self.methods.contains(&method) {
                return Err(PluginError::UnknownMethod(method.to_string()));
            }
            if method == "raw" {
                return Ok(MethodOutput::Raw("<b>hi</b>".to_string()));
            }
            Ok(MethodOutput::Json(json!({
                "plugin": self.name(),
                "method": method,
                "owner_uid": ctx.owner_uid,
                "params": params,
            })))
        })
    }
}

// ── setup ───────────────────────────────────────────────────────────────────

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
        plugins: "auth_internal,dispatchtest".into(),
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

/// A registry holding only the double (production's list is empty until
/// M7.4+), a `Sess` with `uid = 1` + the given CSRF token (as login would).
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
    plugin: DispatchPlugin,
    csrf: &str,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let mut state = AppState::new(test_config(), db);
    let mut host = PluginHost::new();
    host.register(Box::new(plugin));
    state.plugins = Arc::new(host);

    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(100));
    data.set("csrf_token", PValue::Bytes(csrf.as_bytes().to_vec()));
    let sess = Sess::new("test-sid".into());
    sess.replace("test-sid".into(), data);
    (state, sess)
}

fn extra(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// The JSON payload of a `MethodOutput` (panics on `Raw` — the tests that
/// expect JSON fail loudly if a plugin starts printing HTML).
fn json_out(v: &MethodOutput) -> &Value {
    match v {
        MethodOutput::Json(j) => j,
        MethodOutput::Raw(s) => panic!("expected JSON, got raw: {s}"),
        MethodOutput::Response { .. } => panic!("expected JSON, got a response"),
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap()
}

// ── protected path (`op=PluginHandler`) ─────────────────────────────────────

// Dispatch: valid CSRF, existing method → invoked, request map as params.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_dispatch(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["getnote", "setnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "tok123",
        "getnote",
        &extra(&[("plugin", json!("dispatchtest")), ("id", json!(7))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);

    assert_eq!(v["plugin"], "dispatchtest");
    assert_eq!(v["method"], "getnote");
    // The ctx carries the session owner (storage scoping).
    assert_eq!(v["owner_uid"], 1);
    // The method sees the `$_REQUEST`-equivalent: its own op/method, the
    // token, and the extra params.
    assert_eq!(v["params"]["op"], "PluginHandler");
    assert_eq!(v["params"]["method"], "getnote");
    assert_eq!(v["params"]["csrf_token"], "tok123");
    assert_eq!(v["params"]["plugin"], "dispatchtest");
    assert_eq!(v["params"]["id"], 7);
}

// Unknown plugin → `E_UNKNOWN_PLUGIN` (before any method/CSRF check).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_unknown_plugin(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["getnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "tok123",
        "getnote",
        &extra(&[("plugin", json!("no_such_plugin"))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNKNOWN_PLUGIN");
}

// Unknown method → `E_UNKNOWN_METHOD` — and it takes precedence over the
// CSRF gate (the PHP checks `method_exists` before `validate_csrf`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_unknown_method_beats_csrf(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["getnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    // Bad token + unknown method → unknown method wins.
    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "WRONG",
        "no_such_method",
        &extra(&[("plugin", json!("dispatchtest"))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNKNOWN_METHOD");
}

// CSRF fail: bad token, method exists, no `csrf_ignore` → `E_UNAUTHORIZED`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_csrf_fail(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["getnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "WRONG",
        "getnote",
        &extra(&[("plugin", json!("dispatchtest"))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNAUTHORIZED");
}

// `csrf_ignore` bypass: bad token, but the plugin exempts the method →
// invoked.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_csrf_ignore_bypass(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: true,
        methods: vec!["getnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "WRONG",
        "getnote",
        &extra(&[("plugin", json!("dispatchtest"))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(v["method"], "getnote");
}

// `clean()` on the plugin name: tags stripped + trimmed (PHP `clean()`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_name_cleaned(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["getnote"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "tok123",
        "getnote",
        &extra(&[("plugin", json!(" <b> dispatchtest </b> "))]),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(v["plugin"], "dispatchtest");
}

// ── public path (`op=pluginhandler`) ────────────────────────────────────────

// `is_public_method` gating: the method exists but the plugin marks it
// non-public → `E_UNAUTHORIZED`.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_public_private_method_rejected(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: false,
        csrf_ignore: false,
        methods: vec!["getnote"],
    };
    let (state, _sess) = setup(pool, options, plugin, "irrelevant").await;

    let v = ttrss::pluginhost::dispatch::public_dispatch(
        &state,
        "dispatchtest",
        "getnote",
        &json!({}),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNAUTHORIZED");
}

// Public dispatch: `is_public_method` true → invoked with owner 0 (PHP's
// `load($name, KIND_ALL, 0)` — no user context on this path).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_public_dispatch(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["showarticle"],
    };
    let (state, _sess) = setup(pool, options, plugin, "irrelevant").await;

    let v = ttrss::pluginhost::dispatch::public_dispatch(
        &state,
        "dispatchtest",
        "showarticle",
        &json!({ "key": "abc123" }),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(v["plugin"], "dispatchtest");
    assert_eq!(v["method"], "showarticle");
    assert_eq!(v["owner_uid"], 0);
    assert_eq!(v["params"]["key"], "abc123");
}

// Unknown plugin → `E_UNKNOWN_PLUGIN` with the plugin name in the envelope
// (the public path passes `['plugin' => $name]`, the protected one does not).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_public_unknown_plugin(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["showarticle"],
    };
    let (state, _sess) = setup(pool, options, plugin, "irrelevant").await;

    let v = ttrss::pluginhost::dispatch::public_dispatch(
        &state,
        "no_such_plugin",
        "showarticle",
        &json!({}),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNKNOWN_PLUGIN");
    assert_eq!(v["error"]["params"][0]["plugin"], "no_such_plugin");
}

// Unknown method → `E_UNKNOWN_METHOD` (checked before `is_public_method`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_public_unknown_method(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["showarticle"],
    };
    let (state, _sess) = setup(pool, options, plugin, "irrelevant").await;

    let v = ttrss::pluginhost::dispatch::public_dispatch(
        &state,
        "dispatchtest",
        "no_such_method",
        &json!({}),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(code(v), "E_UNKNOWN_METHOD");
}

// `basename(clean(...))` on the plugin name: a path-like `plugin` param
// resolves to its last component (PHP `basename()`).
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_public_name_basename(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: false,
        methods: vec!["showarticle"],
    };
    let (state, _sess) = setup(pool, options, plugin, "irrelevant").await;

    let v = ttrss::pluginhost::dispatch::public_dispatch(
        &state,
        "plugins/dispatchtest",
        "showarticle",
        &json!({}),
    )
    .await
    .expect("no plugin error");
    let v = json_out(&v);
    assert_eq!(v["plugin"], "dispatchtest");
}

// A method that prints raw HTML (PHP's plain `print`) comes back as
// `MethodOutput::Raw` — the response body is the printed text, not a JSON
// envelope.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_protected_raw_output(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let plugin = DispatchPlugin {
        public: true,
        csrf_ignore: true,
        methods: vec!["raw"],
    };
    let (state, sess) = setup(pool, options, plugin, "tok123").await;

    let v = ttrss::pluginhost::dispatch::pluginhandler(
        &state,
        &sess,
        "WRONG",
        "raw",
        &extra(&[("plugin", json!("dispatchtest"))]),
    )
    .await
    .expect("no plugin error");
    match v {
        MethodOutput::Raw(s) => assert_eq!(s, "<b>hi</b>"),
        MethodOutput::Json(j) => panic!("expected Raw, got JSON: {j}"),
        MethodOutput::Response { .. } => panic!("expected Raw, got a response"),
    }
}
