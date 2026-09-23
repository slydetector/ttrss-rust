//! Test: M7.16 — the `toggle_sidebar` plugin (main toolbar button).

mod common;

use test_db::template::with_test_db;

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

async fn setup(url: &str, plugins: &str) -> AppState {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
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
#[tokio::test]
async fn test_toolbar_button() {
    let _ = with_test_db(|url| Box::pin(async move {
    let state = setup(&url, "toggle_sidebar").await;
    let mut host = ctx(&state);
    let plugin = ToggleSidebar;

    plugin.hook_main_toolbar_button(&mut host).await;

    let output = &host.output;
    assert!(output.contains("dijit.form.Button"));
    assert!(output.contains("Plugins.Toggle_Sidebar.toggle"));
    assert!(output.contains("chevron_left"));
    assert!(output.contains("toggle-sidebar-label"));
        Ok(())
    })).await.unwrap();
}

// `get_js` returns the toggle JS.
#[tokio::test]
async fn test_get_js() {
    let _ = with_test_db(|url| Box::pin(async move {
    setup(&url, "toggle_sidebar").await;
    let plugin = ToggleSidebar;

    let js = plugin.get_js();
    assert!(js.contains("Plugins.Toggle_Sidebar"));
    assert!(js.contains("Feeds.toggle"));
    assert!(js.contains("chevron_left"));
    assert!(js.contains("chevron_right"));
        Ok(())
    })).await.unwrap();
}
