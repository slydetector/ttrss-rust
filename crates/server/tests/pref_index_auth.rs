//! DB tests for the `Pref_Prefs::index_auth` handler — the Personal/
//! Authentication tab (a Dojo `TabContainer` with Personal data / Password /
//! App passwords / Authenticator (OTP) panes). Rust-only: the frozen PHP suite
//! has no `Pref_Prefs` auth tests, so these exercise the ported handler on the
//! versioned `schema` + `baseline` fixtures (owner 1 = the session uid).
//!
//! The fidelity bar (AGENTS.md "Frontend HTML forms") is structural: the Dojo
//! widgets and the visible labels / field names / button text must be present;
//! whitespace is not asserted.
mod common;

use ttrss_core::config::Config;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value as PValue;
use ttrss_core::session::Session;
use ttrss_server::session::Sess;
use ttrss_server::AppState;

/// A test-only config (the handlers only need `db`).
fn test_config() -> Config {
    Config {
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_user: "postgres".into(),
        db_pass: "postgres".into(),
        db_name: "postgres".into(),
        db_sslmode: "disable".into(),
        self_url: "http://127.0.0.1:8280/tt-rss/".into(),
        encryption_key: None,
        single_user_mode: false,
        plugins: "auth_internal".into(),
        listen: "127.0.0.1:8281".parse().unwrap(),
        feed_icon_dir: std::path::PathBuf::from("no-such-test-icon-dir"),
        max_favicon_file_size: 1024 * 1024,
        force_article_purge: 0,
        docroot: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tt-rss"),
    }
}

/// Fresh fixtures + a session carrying `uid = 1`, the admin access level, the
/// `auth_internal` module and the `admin` login name.
async fn setup(
    pool: sqlx::pool::PoolOptions<sqlx::Postgres>,
    options: sqlx::postgres::PgConnectOptions,
) -> (AppState, Sess) {
    let db: Db = common::make_db(pool, options).await.expect("test db");
    let state = AppState {
        config: test_config(),
        db,
    };
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    data.set("access_level", PValue::Int(10));
    data.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    data.set("name", PValue::Bytes(b"admin".to_vec()));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// Strip all whitespace so structural comparisons are whitespace-agnostic.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_index_auth_structure(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    let html = ttrss_server::pref_ui::index_auth(&state, &sess)
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
        assert!(n.contains(&format!("title=\"{title}\"")), "missing pane {title}");
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
    let b32 = ttrss_server::otp::base32_upper_unpadded(secret_hex.as_bytes());
    assert!(n.contains(&format!("<code>{b32}</code>")), "missing formatted secret");
}

#[sqlx::test(fixtures(
    "../../testing/fixtures/schema.sql",
    "../../testing/fixtures/baseline.sql"
))]
async fn test_index_auth_otp_enabled(pool: sqlx::pool::PoolOptions<sqlx::Postgres>, options: sqlx::postgres::PgConnectOptions) {
    let (state, sess) = setup(pool, options).await;
    let db = state.db.pool();

    // OTP already enabled → the pane shows the disable form (no QR / secret).
    sqlx::query("UPDATE ttrss_users SET otp_enabled = true WHERE id = 1")
        .execute(db)
        .await
        .expect("enable otp");

    let html = ttrss_server::pref_ui::index_auth(&state, &sess)
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
}
