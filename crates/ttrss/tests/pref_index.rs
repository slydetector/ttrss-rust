//! DB tests for the `Pref_Prefs::index` handler — the settings/preferences tab
//! (the Dojo `AccordionContainer` the preferences tab injects). Rust-only: the
//! frozen PHP suite has no `Pref_Prefs` configuration tests, so these exercise
//! the ported handler on the versioned `schema` + `baseline` fixtures (owner 1
//! = the session uid).
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets and the visible labels / section headers / control types must be
//! present; whitespace is not asserted.
mod common;

use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

/// A test-only config (the handlers only need `db`).
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
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        max_cache_file_size: 64 * 1024 * 1024,
        force_article_purge: 0,
        feed_fetch_timeout: 45,
        feed_fetch_no_cache_timeout: 15,
        max_conditional_interval: 43200,
        http_429_throttle_interval: 3600,
        daemon_unsuccessful_days_limit: 30,
        fetch_extended_url_filtering: true,
        daemon_sleep_interval: 60,
        daemon_max_jobs: 2,
        daemon_feed_limit: 50,
        daemon_update_login_limit: 30,
        cache_max_days: 7,
        schedule_purge_orphans: String::from("@daily"),
        schedule_disk_cache_expire_all: String::from("@daily"),
        schedule_disable_failed_feeds: String::from("@daily"),
        schedule_cleanup_feed_icons: String::from("@daily"),
        schedule_log_daemon_update_login_limit_users: String::from("@daily"),
        schedule_expire_error_log: String::from("@hourly"),
        schedule_expire_lock_files: String::from("@hourly"),
        schedule_send_headlines_digests: String::from("@hourly"),
        docroot: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tt-rss"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1` and an admin access level.
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    (state, common::admin_sess())
}

/// Strip all whitespace so structural comparisons are whitespace-agnostic.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[tokio::test]
async fn test_index_structure() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;

    let html = ttrss::pref_ui::index(&state, &sess).await.expect("render");
    let n = normalize(&html);

    // The accordion + the three panes (titles + the lazy loaders).
    assert!(n.contains("dijit.layout.AccordionContainer"));
    assert!(n.contains("Personaldata/Authentication"));
    assert!(n.contains("Preferences"));
    assert!(n.contains("Plugins"));
    // The auth pane lazy-loads `index_auth` and shows a loading span.
    assert!(n.contains("method:'index_auth'"));
    assert!(n.contains("Loading,pleasewait..."));

    // The Preferences pane form + the save mechanism.
    assert!(n.contains("changeSettingsForm"));
    assert!(n.contains("name=\"method\"value=\"saveconfig\""));

    // All six sections (in order) are present.
    for section in [
        "General",
        "Feeds",
        "Articles",
        "Digest",
        "Advanced",
        "Debugging",
    ] {
        assert!(
            n.contains(&format!("<h2>{section}</h2>")),
            "missing section {section}"
        );
    }

    // Representative field labels (the `short_desc`s) are present.
    for label in [
        "Language",
        "Timezone",
        "Theme",
        "EnableAPI",
        "Defaultupdateinterval",
        "Purgearticlesolderthan",
        "Enablecategories",
        "Enabledigest",
        "Blacklistedtags",
        "ShowarticleandfeedIDs",
    ] {
        assert!(n.contains(label), "missing label {label}");
    }

    // The right control types are emitted for representative prefs.
    assert!(n.contains("dijit.form.FilteringSelect")); // USER_TIMEZONE
    assert!(n.contains("dijit.form.SimpleTextarea")); // BLACKLISTED_TAGS
    assert!(n.contains("dijit.form.NumberSpinner")); // FRESH_ARTICLE_MAX_AGE
    assert!(n.contains("dijit.form.CheckBox")); // booleans
    assert!(n.contains("dijit.form.ValidationTextBox")); // DIGEST_PREFERRED_TIME

    // The `boolean_prefs` hidden tag lists the boolean prefs.
    assert!(n.contains("boolean_prefs"));

    // The Plugins pane (a form whose list loads lazily) + the admin buttons
    // (the session access level is admin).
    assert!(n.contains("changePluginsForm"));
    assert!(n.contains("prefs-plugin-list"));
    assert!(n.contains("Checkforupdates"));
    assert!(n.contains("Installplugin"));
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
async fn test_index_profile_hides_blacklisted() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();

    // Create + activate a named profile (the session `profile` key is set).
    let profile_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_settings_profiles (title, owner_uid) VALUES ('Work', 1) RETURNING id",
    )
    .fetch_one(db)
    .await
    .expect("insert profile");
    {
        let mut data = sess.data();
        data.set("profile", PValue::Int(profile_id as i64));
        sess.replace("test-sid".to_string(), data);
    }

    let html = ttrss::pref_ui::index(&state, &sess).await.expect("render");
    let n = normalize(&html);

    // The profile notice is shown.
    assert!(n.contains("Somepreferencesareonlyavailableindefaultprofile."));
    // A profile-blacklisted pref (USER_TIMEZONE) is hidden in a named profile.
    assert!(!n.contains("CB_USER_TIMEZONE"));
    // Block separators are omitted in a non-default profile (no `<hr/>`).
    assert!(!n.contains("<hr/>"));
    // A non-blacklisted pref is still shown.
    assert!(n.contains("CB_COMBINED_DISPLAY_MODE"));
        Ok(())
    })).await.unwrap();
}
