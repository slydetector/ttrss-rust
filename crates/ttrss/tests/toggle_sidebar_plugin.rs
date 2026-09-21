//! Test: M7.16 — the `toggle_sidebar` plugin (main toolbar button).

mod common;

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::pluginhost::host::HostCtx;
use ttrss::pluginhost::plugin::Plugin;
use ttrss::plugins::toggle_sidebar::ToggleSidebar;
use ttrss::AppState;

fn test_config(plugins: &str) -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = plugins.into();
    cfg
}

async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
    plugins: &str,
) -> AppState {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    AppState::new(test_config(plugins), db)
}

fn ctx(state: &AppState) -> HostCtx<'_> {
    HostCtx::for_user(
        &state.plugins,
        state.db.pool(),
        &state.config,
        Some(1),
        None,
        0,
        &[],
        &["toggle_sidebar"],
    )
}

// `hook_main_toolbar_button` renders the toggle button.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_toolbar_button(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    let state = setup(pool, options, "toggle_sidebar").await;
    let mut host = ctx(&state);
    let plugin = ToggleSidebar;

    plugin.hook_main_toolbar_button(&mut host).await;

    let output = &host.output;
    assert!(output.contains("dijit.form.Button"));
    assert!(output.contains("Plugins.Toggle_Sidebar.toggle"));
    assert!(output.contains("chevron_left"));
    assert!(output.contains("toggle-sidebar-label"));
}

// `get_js` returns the toggle JS.
#[sqlx::test(fixtures(
    "../../test-db/fixtures/schema.sql",
    "../../test-db/fixtures/baseline.sql",
    "../../test-db/fixtures/seed.sql"
))]
async fn test_get_js(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) {
    setup(pool, options, "toggle_sidebar").await;
    let plugin = ToggleSidebar;

    let js = plugin.get_js();
    assert!(js.contains("Plugins.Toggle_Sidebar"));
    assert!(js.contains("Feeds.toggle"));
    assert!(js.contains("chevron_left"));
    assert!(js.contains("chevron_right"));
}
