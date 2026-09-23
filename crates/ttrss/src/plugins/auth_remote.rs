//! M7.20 (Phase 7) — the **`auth_remote`** plugin (authenticate against
//! external passwords: proxy HTTP Basic auth or client SSL certificate).
//!
//! PHP reference: `tt-rss/plugins/auth_remote/init.php` (extends
//! `Auth_Base`).
//!
//! System plugin (`about().system = true`) implementing `IAuthModule`
//! (api v1 — no `change_password`):
//! - `authenticate` — the login is the proxy-supplied user (the first
//!   non-empty of `REMOTE_USER`, `HTTP_REMOTE_USER`,
//!   `REDIRECT_REMOTE_USER`, `PHP_AUTH_USER`), else the client-SSL
//!   certificate's registered serial (`get_login_by_ssl_certificate`),
//!   else failure. The form's `login`/`password` are ignored (PHP: the
//!   password arg is only used to seed a new auto-created account).
//!   On success the account is auto-created when missing
//!   (`Config::AUTH_AUTO_CREATE`, default on — `Auth_Base::auto_create_user`)
//!   and the session carries the `fake_login`/`fake_password`/`hide_hello`/
//!   `hide_logout` markers; with auto-create on, the LemonLDAP name/email
//!   headers (`HTTP_USER_NAME`/`AUTHENTICATE_CN`, `HTTP_USER_MAIL`/
//!   `AUTHENTICATE_MAIL`) update the account.
//! - `hook_post_logout` — registered only when
//!   `AUTH_REMOTE_POST_LOGOUT_URL` is set: returns `[url]`, the logout
//!   handler's redirect target (validated; empty → the default index).

use crate::core::user::{get_salt, hash_password, ALGO_SSHA512};
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::core::config::Config;
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::{HOOK_AUTH_USER, HOOK_POST_LOGOUT};
use serde_json::Value;
use sqlx::Row;

/// The `$_SERVER` names `authenticate` tries, in order (PHP `init.php`).
const PROXY_USER_HEADERS: [&str; 4] = [
    "REMOTE_USER",
    "HTTP_REMOTE_USER",
    "REDIRECT_REMOTE_USER",
    "PHP_AUTH_USER",
];

/// PHP's `class Auth_Remote extends Auth_Base`.
#[derive(Clone, Copy)]
pub struct AuthRemote;

impl Plugin for AuthRemote {
    fn name(&self) -> &'static str {
        "auth_remote"
    }
    fn class_name(&self) -> &'static str {
        "Auth_Remote"
    }
    fn about(&self) -> About {
        About {
            version: None,
            description:
                "Authenticates against external passwords (HTTP Authentication, SSL certificates)"
                    .into(),
            author: "fox".into(),
            system: true,
            more_info: String::new(),
        }
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_AUTH_USER);
        // `Config::add(AUTH_REMOTE_POST_LOGOUT_URL, "", T_STRING)` + the
        // conditional HOOK_POST_LOGOUT registration: the port's `init`
        // runs without a `Config` (the substrate contract), so the
        // registration is unconditional (the port's `Config` always
        // carries the value; the hook is a no-op redirect-fallback when
        // the URL is empty). The logout flow's chain validates the result,
        // matching PHP's `!empty($result[0])` guard.
        host.add_hook_default(HOOK_POST_LOGOUT);
    }

    /// `Auth_Remote::authenticate($login, $password, $service)` — the
    /// proxy/cert login. The form's `login`/`password` are unused (the
    /// password seeds only a new auto-created account).
    fn authenticate<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        _login: &'a str,
        password: &'a str,
        _service: &'a str,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(async move {
            let mut try_login = String::new();
            for name in PROXY_USER_HEADERS {
                if let Some(v) = host.server(name).filter(|v| !v.is_empty()) {
                    try_login = v.to_ascii_lowercase();
                    break;
                }
            }
            if try_login.is_empty() {
                try_login = get_login_by_ssl_certificate(host).await;
            }
            if try_login.is_empty() {
                return None;
            }

            let Some(uid) = auto_create_user(host, &try_login, password).await else {
                return None;
            };

            // The fake-login session markers (PHP `$_SESSION[...] = …`):
            // the port sets them on the session the caller builds from the
            // returned uid — see `login_flow`'s post-auth hook. Model it
            // here on the HostCtx so the caller can copy them.
            host.fake_login = Some(try_login.clone());

            // LemonLDAP extras (only with auto-create on, PHP:
            // `Config::get(Config::AUTH_AUTO_CREATE)`).
            if host.cfg.auth_auto_create {
                if let Some(fullname) = host
                    .server("HTTP_USER_NAME")
                    .or_else(|| host.server("AUTHENTICATE_CN"))
                    .filter(|v| !v.is_empty())
                {
                    let _ = sqlx::query("UPDATE ttrss_users SET full_name = $1 WHERE id = $2")
                        .bind(fullname)
                        .bind(uid)
                        .execute(host.pool)
                        .await;
                }
                if let Some(email) = host
                    .server("HTTP_USER_MAIL")
                    .or_else(|| host.server("AUTHENTICATE_MAIL"))
                    .filter(|v| !v.is_empty())
                {
                    let _ = sqlx::query("UPDATE ttrss_users SET email = $1 WHERE id = $2")
                        .bind(email)
                        .bind(uid)
                        .execute(host.pool)
                        .await;
                }
            }

            Some(uid)
        })
    }

    /// `Auth_Remote::hook_post_logout($login, $user_id)` — `[0]` is the
    /// configured redirect URL.
    fn hook_post_logout<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        _login: &'a str,
        _user_id: i32,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
            let url = host.cfg.auth_remote_post_logout_url.clone();
            Value::Array(vec![Value::String(url)])
        })
    }
}

/// `Auth_Base::auto_create_user($login, $password)`: the login's user id —
/// the existing account, or a fresh one (salt + random password, access 0)
/// when `AUTH_AUTO_CREATE` is on and the login is unknown.
async fn auto_create_user(host: &mut HostCtx<'_>, login: &str, password: &str) -> Option<i32> {
    let cfg: &Config = host.cfg;
    if !login.is_empty() && cfg.auth_auto_create {
        let existing: Option<i32> =
            sqlx::query_scalar("SELECT id FROM ttrss_users WHERE login = $1")
                .bind(login)
                .fetch_optional(host.pool)
                .await
                .ok()
                .flatten();
        if existing.is_none() {
            let salt = get_salt();
            let pwd = if password.is_empty() {
                crate::util::make_password(12)
            } else {
                password.to_string()
            };
            let hash = hash_password(&pwd, &salt, ALGO_SSHA512)?;
            let id: Option<i32> =
                sqlx::query_scalar(
                    "INSERT INTO ttrss_users (salt, login, pwd_hash, access_level, created) \
                     VALUES ($1, $2, $3, 0, NOW()) RETURNING id",
                )
                .bind(&salt)
                .bind(login)
                .bind(hash)
                .fetch_optional(host.pool)
                .await
                .ok()
                .flatten();
            if id.is_some() {
                return id;
            }
        }
        return existing;
    }
    sqlx::query_scalar("SELECT id FROM ttrss_users WHERE login = $1")
        .bind(login)
        .fetch_optional(host.pool)
        .await
        .ok()
        .flatten()
}

/// `Auth_Remote::get_login_by_ssl_certificate`: the login whose
/// `SSL_CERT_SERIAL` pref matches the current client certificate's
/// fingerprint (`Pref_Prefs::_get_ssl_certificate_id`: the sha1 of the
/// mod_ssl serial/start/end/subject-`DN` concatenation, the
/// `REDIRECT_*` (subrequest) pair tried first).
async fn get_login_by_ssl_certificate(host: &mut HostCtx<'_>) -> String {
    let serial = {
        for prefix in ["REDIRECT_SSL_CLIENT_", "SSL_CLIENT_"] {
            let serial = host
                .server(&format!("{prefix}M_SERIAL"))
                .unwrap_or("")
                .to_string();
            if !serial.is_empty() {
                let v_start = host
                    .server(&format!("{prefix}V_START"))
                    .unwrap_or("")
                    .to_string();
                let v_end = host
                    .server(&format!("{prefix}V_END"))
                    .unwrap_or("")
                    .to_string();
                let s_dn = host
                    .server(&format!("{prefix}S_DN"))
                    .unwrap_or("")
                    .to_string();
                return crate::util::sha1_hex(&format!("{serial}{v_start}{v_end}{s_dn}"));
            }
        }
        String::new()
    };
    if serial.is_empty() {
        return String::new();
    }
    sqlx::query(
        "SELECT u.login FROM ttrss_user_prefs2 p, ttrss_users u
         WHERE p.pref_name = 'SSL_CERT_SERIAL' AND p.value = $1 AND
         p.owner_uid = u.id",
    )
    .bind(&serial)
    .fetch_optional(host.pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("login").ok())
    .unwrap_or_default()
}
