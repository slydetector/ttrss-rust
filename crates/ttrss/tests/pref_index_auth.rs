//! DB tests for the `Pref_Prefs::index_auth` handler — the Personal/
//! Authentication tab (a Dojo `TabContainer` with Personal data / Password /
//! App passwords / Authenticator (OTP) panes). Rust-only: the frozen PHP suite
//! has no `Pref_Prefs` auth tests, so these exercise the ported handler on the
//! versioned `schema` + `baseline` fixtures (owner 1 = the session uid).
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets and the visible labels / field names / button text must be present;
//! whitespace is not asserted.
use crate::common;

use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;

fn test_config() -> Config {
    let mut c = common::test_config();
    c.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    c
}

/// Fresh fixtures + a session carrying `uid = 1`, the admin access level, the
/// `auth_internal` module and the `admin` login name.
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set(
        "access_level",
        PValue::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64),
    );
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    sess.set_self_url(&state.config.self_url_rtrim());
    (state, sess)
}

/// Strip all whitespace so structural comparisons are whitespace-agnostic.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[tokio::test]
async fn test_index_auth_structure() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = state.db.pool();

            let html = ttrss::pref_ui::index_auth(&state, &sess)
                .await
                .expect("render");
            let n = normalize(&html);

            // The tab container + the four pane titles.
            assert!(n.contains("dijit.layout.TabContainer"));
            for title in [
                "Personaldata",
                "Password",
                "Apppasswords",
                "Authenticator(OTP)",
            ] {
                assert!(
                    n.contains(&format!("title=\"{title}\"")),
                    "missing pane {title}"
                );
            }

            // Personal data pane: form + fields + Save.
            assert!(n.contains("changePersonalData"));
            assert!(n.contains("name='full_name'"));
            assert!(n.contains("name='email'"));
            assert!(n.contains("Save"));

            // Password pane (auth_internal implements IAuthModule2).
            assert!(n.contains("changepassword"));
            assert!(n.contains("name='old_password'"));
            assert!(n.contains("name='new_password'"));
            assert!(n.contains("name='confirm_password'"));
            assert!(n.contains("pwd_change_infobox"));
            assert!(n.contains("Changepassword"));

            // App passwords pane: holder + the two AJAX buttons.
            assert!(n.contains("app_passwords_holder"));
            assert!(n.contains("Helpers.AppPasswords.generate()"));
            assert!(n.contains("Helpers.AppPasswords.removeSelected()"));
            assert!(n.contains("Generatepassword"));
            assert!(n.contains("Removeselected"));

            // OTP disabled branch: QR image + formatted secret + enable form.
            assert!(n.contains("data:image/png;base64,"), "missing QR data URI");
            assert!(n.contains("otpenable"));
            assert!(n.contains("OTPsecret:"));
            assert!(n.contains("name='otp'"));
            assert!(n.contains("EnableOTP"));
            // No disable-form in the disabled branch.
            assert!(!n.contains("otpdisable"));

            // The generated secret is persisted (10 random bytes = 20 hex chars) and
            // the pane shows its base32 form (unpadded) in the `<code>`.
            let secret_hex: String =
                sqlx::query_scalar("SELECT otp_secret FROM ttrss_users WHERE id = 1")
                    .fetch_one(db)
                    .await
                    .expect("otp_secret");
            assert_eq!(secret_hex.len(), 20, "unexpected otp_secret: {secret_hex}");
            assert!(secret_hex.chars().all(|c| c.is_ascii_hexdigit()));
            let b32 = ttrss::otp::base32_upper_unpadded(secret_hex.as_bytes());
            assert!(
                n.contains(&format!("<code>{b32}</code>")),
                "missing formatted secret"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_index_auth_otp_enabled() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let db = state.db.pool();

            // OTP already enabled → the pane shows the disable form (no QR / secret).
            sqlx::query("UPDATE ttrss_users SET otp_enabled = true WHERE id = 1")
                .execute(db)
                .await
                .expect("enable otp");

            let html = ttrss::pref_ui::index_auth(&state, &sess)
                .await
                .expect("render");
            let n = normalize(&html);

            assert!(n.contains("otpdisable"));
            assert!(n.contains("DisableOTP"));
            assert!(n.contains("name='password'"));
            assert!(n.contains("Onetimepasswordsarecurrentlyenabled"));
            // The disabled-branch artifacts are absent.
            assert!(!n.contains("otpenable"));
            assert!(!n.contains("data:image/png;base64,"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `index_auth` under a `de_DE` scope — the pane titles and form labels the
/// PHP wraps in `__()` render from the German catalog (regression: the port
/// had them as bare literals, so they stayed English under a locale).
#[tokio::test]
async fn test_index_auth_localizes_de_de() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sess) = setup(&url).await;
            let ctx = ttrss::i18n::I18nCtx {
                locale: Some("de_DE".to_string()),
                cache: state.i18n.clone(),
            };
            let html = ttrss::i18n::I18N
                .scope(ctx, async {
                    ttrss::pref_ui::index_auth(&state, &sess).await
                })
                .await
                .expect("render");

            // Pane titles (the German catalog).
            assert!(
                html.contains("title=\"Persönliche Daten\""),
                "German pane title"
            );
            assert!(html.contains("title=\"Passwort\""), "German pane title");
            assert!(
                html.contains("title=\"App-Passwörter\""),
                "German pane title"
            );
            assert!(
                html.contains("title=\"Authentifizierung (OTP)\""),
                "German pane title"
            );

            // Personal data pane labels + button.
            assert!(
                html.contains("Vollständiger Name:"),
                "German full-name label"
            );
            assert!(html.contains("Speichern"), "German save button");

            // Password pane labels + button.
            assert!(
                html.contains("Altes Passwort:"),
                "German old-password label"
            );
            assert!(
                html.contains("Neues Passwort:"),
                "German new-password label"
            );
            assert!(
                html.contains("Passwort bestätigen:"),
                "German confirm label"
            );
            assert!(html.contains("Passwort ändern"), "German change button");

            // App passwords pane buttons.
            assert!(
                html.contains("Passwort generieren"),
                "German generate button"
            );

            // OTP pane (disabled branch): labels + enable button.
            assert!(html.contains("OTP Geheimnis:"), "German secret label");
            assert!(html.contains("Verifizierungscode:"), "German code label");
            assert!(
                html.contains("Einmalpasswörter einschalten"),
                "German enable button"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
