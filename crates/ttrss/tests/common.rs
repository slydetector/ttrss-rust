//! Shared setup for the DB-backed parity test binaries (the test-parity
//! program in `docs/01-plan.md`, rows 8–14).
//!
//! The `#[ctor]` function (which runs before the test harness's `main`)
//! starts/attaches to the dedicated test Postgres, exports `DATABASE_URL`,
//! and ensures the versioned template database
//! ([`test_db::template::ensure_template`]). Each test then calls
//! [`test_db::template::with_test_db`], which clones a fresh versioned copy
//! of the template (schema + baseline + `seed.sql` already applied) per
//! test and drops it when the test finishes.
//!
//! [`make_db_from_url`] wraps the clone's URL in a core [`Db`] with the
//! server's exact per-connection session setup
//! (`ttrss::core::db::SESSION_SETUP`: `TIME ZONE 0`,
//! `datestyle = 'ISO, european'`, …) so test connections behave exactly
//! like the server's and the PHP reference's.

// Every test binary links this module but uses only a subset of the helpers,
// so dead-code linting per binary is noise by design.
#![allow(dead_code)]

use sqlx::AssertSqlSafe;
use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgConnection, Postgres};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ttrss::core::db::{Db, SESSION_SETUP};

#[ctor::ctor(unsafe)]
fn init_test_pg() {
    // Mirror the PHP integration-test environment: with this set,
    // `urlhelper::validate` skips the extended (safe-port / disallowed-IP)
    // block, so these DB-backed tests can fetch loopback mock feeds
    // (`http://127.0.0.1:<port>/…`). `tests/url_validate.rs` does NOT link
    // this module, so it runs with the flag unset and still exercises the
    // extended rejections.
    unsafe { std::env::set_var("IS_INTEGRATION_TESTING", "1") };
    test_db::init_database_url();
    // Build the shared template DB (no-op if already current).
    let url = test_db::master_url();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("ctor: build tokio runtime");
    rt.block_on(test_db::template::ensure_template(&url))
        .unwrap_or_else(|e| panic!("ctor: template setup failed: {e}"));
}

/// Create a [`Db`] from a database URL (for use with [`test_db::with_test_db`]).
///
/// Applies the PHP per-connection session setup (statement cache disabled,
/// `SESSION_SETUP` on every connection).
pub async fn make_db_from_url(url: &str) -> anyhow::Result<Db> {
    use url::Url;
    let u: Url = url.parse()?;
    let mut opts = PgConnectOptions::new().statement_cache_capacity(0);
    if let Some(host) = u.host_str() {
        opts = opts.host(host);
    }
    if let Some(port) = u.port() {
        opts = opts.port(port);
    }
    let user = u.username();
    if !user.is_empty() {
        opts = opts.username(user);
    }
    if let Some(pwd) = u.password() {
        opts = opts.password(pwd);
    }
    let db = u.path().trim_start_matches('/');
    if !db.is_empty() {
        opts = opts.database(db);
    }

    let pool = PoolOptions::<Postgres>::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(120))
        .before_acquire(|conn: &mut PgConnection, _meta| {
            Box::pin(async move {
                for stmt in SESSION_SETUP {
                    sqlx::query(AssertSqlSafe(*stmt))
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(true)
            })
        })
        .connect_with(opts)
        .await?;
    Ok(Db::from_pool(pool))
}

// ── Consolidated test helpers (replaces per-file test_config + setup) ──────

/// Standard test [`Config`]: `Config::for_test()` with the default test plugin.
pub fn test_config() -> ttrss::core::config::Config {
    let mut cfg = ttrss::core::config::Config::for_test();
    cfg.plugins = "auth_internal".into();
    cfg
}

/// Test config + a per-test temp cache dir, deleted when the guard drops.
///
/// Tests that run the update pipeline must use this: favicon writes go to
/// `<cache_dir>/feed-icons/<feed_id>` and would otherwise be left on the
/// filesystem. The guard owns the dir; clone `cfg` for the test body.
pub struct TestCfg {
    pub cfg: ttrss::core::config::Config,
    _cache: tempfile::TempDir,
}

impl TestCfg {
    pub fn new() -> Self {
        let cache = tempfile::tempdir().expect("per-test cache dir");
        let mut cfg = test_config();
        cfg.cache_dir = cache.path().to_path_buf();
        Self { cfg, _cache: cache }
    }

    pub fn cache_dir(&self) -> &std::path::Path {
        self._cache.path()
    }
}

pub fn test_cfg() -> TestCfg {
    TestCfg::new()
}

/// The fixture admin's stored `pwd_hash` (the base `schema.sql`: SHA1 of
/// "password" — deliberately legacy, pre-SSHA-512, so login-upgrade paths
/// are testable).
pub const ADMIN_PWD_HASH: &str = "SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8";

/// Standard admin session: `uid = 1`, `access_level = 10`, the fixture
/// admin's `pwd_hash` (session validation compares it against the DB row).
pub fn admin_sess() -> ttrss::session::Sess {
    use ttrss::core::phpser::Value as PValue;
    use ttrss::core::session::Session;
    use ttrss::session::Sess;
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set(
        "pwd_hash",
        PValue::Bytes(ADMIN_PWD_HASH.as_bytes().to_vec()),
    );
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess
}

/// Standard test state: [`AppState`] + admin [`Sess`] (the most common pattern).
pub async fn make_state(url: &str) -> (ttrss::AppState, ttrss::session::Sess) {
    let db = make_db_from_url(url).await.expect("test db");
    let state = ttrss::AppState::new(test_config(), db);
    (state, admin_sess())
}

/// [`AppState`] only (for tests that build their own session).
pub async fn make_app_state(url: &str) -> ttrss::AppState {
    let db = make_db_from_url(url).await.expect("test db");
    ttrss::AppState::new(test_config(), db)
}

/// [`Db`] only (for tests that just need database access).
pub async fn make_db_ref(url: &str) -> Db {
    make_db_from_url(url).await.expect("test db")
}
// ── Loopback mock HTTP server (shared by the fetch / on-demand-update tests) ─

/// A canned HTTP response served by [`MockServer`].
#[derive(Clone)]
#[allow(dead_code)]
pub struct Canned {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// A minimal loopback HTTP/1.1 server (the origin). Serves canned responses in
/// order, repeating the last when exhausted.
#[allow(dead_code)]
pub struct MockServer {
    port: u16,
    responses: Arc<std::sync::Mutex<Vec<Canned>>>,
    _thread: std::thread::JoinHandle<()>,
}

#[allow(dead_code)]
impl MockServer {
    pub fn new(responses: Vec<Canned>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses = Arc::new(Mutex::new(responses));
        let count = Arc::new(AtomicUsize::new(0));
        let r = responses.clone();
        let c = count.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Read the request (its contents are irrelevant).
                let mut data: Vec<u8> = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            data.extend_from_slice(&buf[..n]);
                            if data.windows(4).any(|w| w == b"\r\n\r\n") || data.len() > 65536 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let idx = c.fetch_add(1, Ordering::Relaxed);
                let resp = {
                    let resps = r.lock().unwrap();
                    resps[idx.min(resps.len().saturating_sub(1))].clone()
                };
                let mut have_cl = false;
                let mut s = format!("HTTP/1.1 {} {}\r\n", resp.status, resp.reason);
                for (k, v) in &resp.headers {
                    s.push_str(&format!("{k}: {v}\r\n"));
                    if k.eq_ignore_ascii_case("content-length") {
                        have_cl = true;
                    }
                }
                if !have_cl {
                    s.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
                }
                s.push_str("Connection: close\r\n\r\n");
                let mut out = s.into_bytes();
                out.extend_from_slice(&resp.body);
                let _ = stream.write_all(&out);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        Self {
            port,
            responses,
            _thread: thread,
        }
    }

    /// Replace the canned response at `idx` (before any request is served).
    pub fn set_response(&self, idx: usize, resp: Canned) {
        let mut r = self.responses.lock().unwrap();
        r[idx] = resp;
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}
