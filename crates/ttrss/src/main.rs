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

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::http;
use ttrss::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let db = Db::connect(&config).await?;
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
