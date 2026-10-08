//! `ttrss` — the Rust HTTP server for Tiny Tiny RSS (M1 walking skeleton).
//!
//! Serves, under the configured base path (default `/tt-rss`):
//!   * static assets byte-for-byte from the doc root,
//!   * the login page (GET `/` and `/index.php` when not authenticated),
//!   * the login `POST` (`public.php?op=login`), including the TOTP challenge,
//!   * DB-backed encrypted sessions (read per request, persisted when changed),
//!   * the authenticated SPA shell (GET `/` and `/index.php`),
//!   * `/healthz`.
//!
//! The HTTP layer (router + handlers) lives in the library crate at
//! `ttrss::http` so integration tests can drive it; this binary only boots.

use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::http;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // The official startup.sh's VAR__FILE docker-secret expansion, in-process
    // (parity, done before any env read — see the function's doc).
    ttrss::core::config::expand_file_secrets();

    let config = Config::from_env()?;
    // Privilege drop (docs/19): as root, chown the data dir and drop to
    // OWNER_UID (default 1000) before any I/O, so the DB pool, caches and
    // the listener all run as the app user. A no-op when started non-root
    // (dev, tests, `docker run --user`).
    ttrss::privs::drop_privileges(&config.data_dir, config.owner_uid, config.owner_gid)?;
    let db = Db::connect(&config).await?;
    // Schema bootstrap/migration (docs/20): runs on every startup, before
    // the daemon and the listener — the server only opens for business on
    // a schema it knows (fresh DB → full schema; older DB → numbered
    // migrations). Fail-closed: an error exits before binding.
    ttrss::core::schema::ensure_up_to_date(&config, &db).await?;
    // The in-process first-boot bootstrap (docs/19 P0): the reference
    // startup.sh's user-management steps — admin-password policy (stock
    // `password` → random 16-char + star banner), admin access level,
    // auto-create user. Env-driven, idempotent, a no-op when the vars are
    // unset; fail-closed like the schema step.
    ttrss::bootstrap::run(
        &db,
        &ttrss::bootstrap::BootstrapVars::from_env(),
        &config.data_dir,
    )
    .await?;
    tracing::info!(
        listen = %config.listen,
        docroot = %config.docroot.display(),
        base = %config.base_path(),
        "starting ttrss"
    );

    let state = AppState::new(config.clone(), db);
    // The M6c.3 daemon/scheduler loop, hosted in this process (D1 = B):
    // the background feed-update batches + the cron housekeeping tasks.
    // `TTRSS_DAEMON=0` skips it (env-only switch — the PHP daemon is a
    // separate process that simply isn't run; the record/replay harness
    // needs a static DB, so it runs with this off).
    let daemon_enabled = std::env::var("TTRSS_DAEMON")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| ttrss::core::config::sql_bool_to_bool(&v))
        .unwrap_or(true);
    let cancel = tokio_util::sync::CancellationToken::new();
    if daemon_enabled {
        {
            let pool = state.db.pool().clone();
            let cfg = state.config.clone();
            let plugins = state.plugins.clone();
            let loop_cancel = cancel.clone();
            tokio::spawn(async move {
                ttrss::updates::daemon_loop(&pool, &cfg, plugins, loop_cancel).await;
            });
        }
        tracing::info!(
            interval_secs = state.config.daemon_sleep_interval,
            max_jobs = state.config.daemon_max_jobs,
            "daemon loop started"
        );
    } else {
        tracing::info!("daemon loop disabled (TTRSS_DAEMON=0)");
    }

    let app = http::build_router(&state).with_state(state);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(addr = %config.listen, "listening");
    let shutdown_cancel = cancel.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Ctrl-C / SIGTERM: stop accepting new requests and let the
            // update loop finish its in-flight batch.
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
            shutdown_cancel.cancel();
        })
        .await?;
    Ok(())
}
