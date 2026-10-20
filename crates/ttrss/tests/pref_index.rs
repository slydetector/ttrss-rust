//! DB tests for the `Pref_Prefs::index` handler — the settings/preferences tab
//! (the Dojo `AccordionContainer` the preferences tab injects). Rust-only: the
//! frozen PHP suite has no `Pref_Prefs` configuration tests, so these exercise
//! the ported handler on the versioned `schema` + `baseline` fixtures (owner 1
//! = the session uid).
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets and the visible labels / section headers / control types must be
//! present; whitespace is not asserted.
use crate::common;

use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::session::Sess;

fn test_config() -> Config {
    let mut c = common::test_config();
    c.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    c
}

/// Fresh fixtures + a session carrying `uid = 1` and an admin access level.
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    (state, common::admin_sess())
}

/// Strip all whitespace so structural comparisons are whitespace-agnostic.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[tokio::test]
async fn test_index_structure() {
    with_test_db(|url| {
        Box::pin(async move {
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
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_index_profile_hides_blacklisted() {
    with_test_db(|url| {
        Box::pin(async move {
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
        })
    })
    .await
    .unwrap();
}

/// M10.3: the preferences tab localizes its section titles, field labels, and
/// the `<strong>hours</strong>` help composite under a `de_DE` locale (the bare
/// `__`/`t_sprintf` helpers read the per-request `I18N` scope). Rust-only.
#[tokio::test]
async fn test_index_localizes_de_de() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let ctx = ttrss::i18n::I18nCtx {
                locale: Some("de_DE".to_string()),
                cache: state.i18n.clone(),
            };
            let html = ttrss::i18n::I18N
                .scope(ctx, async { ttrss::pref_ui::index(&state, &sess).await })
                .await
                .expect("render");
            assert!(
                html.contains("<h2>Allgemein</h2>"),
                "General section must be German; head: {}",
                &html[..html.len().min(400)]
            );
            assert!(
                html.contains("<h2>Artikel</h2>"),
                "Articles section must be German"
            );
            assert!(
                html.contains("Aktiviere Zusammenfassung"),
                "Enable digest label must be German"
            );
            assert!(
                html.contains("Gesperrte Tags"),
                "Blacklisted tags label must be German"
            );
            assert!(
                html.contains("<strong>Stunden</strong>"),
                "hours help composite must be German"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
