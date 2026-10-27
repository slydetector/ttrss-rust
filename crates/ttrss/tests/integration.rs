//! Merged test binary: the 80 `common`-linked DB-backed parity suites
//! (one link instead of 80). This toolchain resolves `mod x;` in a
//! `tests/name.rs` target only via an explicit `#[path]`, so the
//! suites stay flat in tests/. `autotests = false` in Cargo.toml
//! keeps the flat files from reappearing as separate binaries.

#[path = "af_comics_fetch_hooks.rs"]
mod af_comics_fetch_hooks;
#[path = "af_psql_trgm_plugin.rs"]
mod af_psql_trgm_plugin;
#[path = "api_getcategories.rs"]
mod api_getcategories;
#[path = "api_getconfig.rs"]
mod api_getconfig;
#[path = "api_getfeeds.rs"]
mod api_getfeeds;
#[path = "api_getheadlines_params.rs"]
mod api_getheadlines_params;
#[path = "api_getheadlines_sandbox.rs"]
mod api_getheadlines_sandbox;
#[path = "api_render_article_api.rs"]
mod api_render_article_api;
#[path = "api_writes.rs"]
mod api_writes;
#[path = "app_passwords.rs"]
mod app_passwords;
#[path = "article_handler_parity.rs"]
mod article_handler_parity;
#[path = "article_parity.rs"]
mod article_parity;
#[path = "auth_remote_plugin.rs"]
mod auth_remote_plugin;
#[path = "auth_writes.rs"]
mod auth_writes;
#[path = "batch_smoke.rs"]
mod batch_smoke;
#[path = "bookmarklets_plugin.rs"]
mod bookmarklets_plugin;
#[path = "bootstrap.rs"]
mod bootstrap;
#[path = "cache_starred_images_plugin.rs"]
mod cache_starred_images_plugin;
#[path = "catchup_scope.rs"]
mod catchup_scope;
mod common;
#[path = "counters_parity.rs"]
mod counters_parity;
#[path = "daemon_loop.rs"]
mod daemon_loop;
#[path = "digest.rs"]
mod digest;
#[path = "dispatch_completion.rs"]
mod dispatch_completion;
#[path = "enclosure_chains.rs"]
mod enclosure_chains;
#[path = "feed_crud.rs"]
mod feed_crud;
#[path = "feed_edit.rs"]
mod feed_edit;
#[path = "feed_pipeline_hooks.rs"]
mod feed_pipeline_hooks;
#[path = "feeds_parity.rs"]
mod feeds_parity;
#[path = "fetch_one_feed.rs"]
mod fetch_one_feed;
#[path = "filter_crud.rs"]
mod filter_crud;
#[path = "filter_editrule.rs"]
mod filter_editrule;
#[path = "filter_i18n.rs"]
mod filter_i18n;
#[path = "filter_index.rs"]
mod filter_index;
#[path = "filter_read.rs"]
mod filter_read;
#[path = "filter_test.rs"]
mod filter_test;
#[path = "forgotpass.rs"]
mod forgotpass;
#[path = "headlines_parity.rs"]
mod headlines_parity;
#[path = "housekeeping.rs"]
mod housekeeping;
#[path = "i18n_e2e.rs"]
mod i18n_e2e;
#[path = "images_disk_cache.rs"]
mod images_disk_cache;
#[path = "importdb.rs"]
mod importdb;
#[path = "label_crud.rs"]
mod label_crud;
#[path = "labels_parity.rs"]
mod labels_parity;
#[path = "no_iframes_plugin.rs"]
mod no_iframes_plugin;
#[path = "note_plugin.rs"]
mod note_plugin;
#[path = "nsfw_plugin.rs"]
mod nsfw_plugin;
#[path = "on_demand_updates.rs"]
mod on_demand_updates;
#[path = "opml_css_roundtrip.rs"]
mod opml_css_roundtrip;
#[path = "opml_export.rs"]
mod opml_export;
#[path = "opml_import.rs"]
mod opml_import;
#[path = "plugin_dispatch.rs"]
mod plugin_dispatch;
#[path = "plugin_routes.rs"]
mod plugin_routes;
#[path = "plugin_storage.rs"]
mod plugin_storage;
#[path = "pref_feeds_labels_index.rs"]
mod pref_feeds_labels_index;
#[path = "pref_index.rs"]
mod pref_index;
#[path = "pref_index_auth.rs"]
mod pref_index_auth;
#[path = "pref_profiles.rs"]
mod pref_profiles;
#[path = "pref_saveconfig.rs"]
mod pref_saveconfig;
#[path = "pref_system.rs"]
mod pref_system;
#[path = "pref_users_admin.rs"]
mod pref_users_admin;
#[path = "prefs_parity.rs"]
mod prefs_parity;
#[path = "prefs_tabs_plugin.rs"]
mod prefs_tabs_plugin;
#[path = "public_getprofiles.rs"]
mod public_getprofiles;
#[path = "public_http.rs"]
mod public_http;
#[path = "response_compression.rs"]
mod response_compression;
#[path = "rpc_config_verbs.rs"]
mod rpc_config_verbs;
#[path = "rpc_init_translations.rs"]
mod rpc_init_translations;
#[path = "rpc_parity.rs"]
mod rpc_parity;
#[path = "schema_migrations.rs"]
mod schema_migrations;
#[path = "search_scope.rs"]
mod search_scope;
#[path = "search_to_sql.rs"]
mod search_to_sql;
#[path = "session_parity.rs"]
mod session_parity;
#[path = "share_plugin.rs"]
mod share_plugin;
#[path = "shell_locale.rs"]
mod shell_locale;
#[path = "toggle_sidebar_plugin.rs"]
mod toggle_sidebar_plugin;
#[path = "update_rss_feed.rs"]
mod update_rss_feed;
#[path = "user_parity.rs"]
mod user_parity;
#[path = "user_verbs.rs"]
mod user_verbs;
#[path = "view_labels.rs"]
mod view_labels;
#[path = "view_method.rs"]
mod view_method;
#[path = "view_pipeline_hooks.rs"]
mod view_pipeline_hooks;
