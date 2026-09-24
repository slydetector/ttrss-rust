//! Port of the `auth_internal` plugin's web-login path (service == "") plus the
//! `UserHelper::authenticate` result semantics.
//!
//! Web login flow (matches `Auth_Internal::authenticate`):
//!   * if the user has OTP enabled:
//!       - otp supplied (non-zero)  → verify password AND otp → success/failure
//!       - otp absent / zero        → render the OTP challenge page
//!   * otherwise:
//!       - throttle if the last failed attempt is within `AUTH_MIN_INTERVAL`
//!       - verify the password → success/failure (stamping `last_auth_attempt`)
//!
//! On success `UserHelper::authenticate` additionally:
//!   * upgrades the stored `pwd_hash` to the current algorithm (SSHA-512) if
//!     it is stored in an older one (`Auth_Internal::check_password`), and
//!   * stamps `ttrss_users.last_login = NOW()`.

use crate::core::config::SCHEMA_VERSION;use crate::i18n::{__};

use crate::core::db::Db;
use crate::core::phpser::Value;
use crate::core::session::{self as core_session, Session};
use crate::core::user;
use crate::util::{now_unix, random_hex};
use chrono::NaiveDateTime;
use sqlx::Row;

/// `AUTH_MIN_INTERVAL` (seconds) — minimum gap between failed login attempts.
pub const AUTH_MIN_INTERVAL: i64 = 5;

const ERR_BAD_CREDENTIALS: &str = "Incorrect username or password";
const ERR_THROTTLED: &str = "Too many authentication attempts, throttled.";

/// The subset of `ttrss_users` needed to authenticate a web login.
struct UserRow {
    id: i32,
    login: String,
    access_level: i32,
    pwd_hash: String,
    salt: String,
    otp_enabled: bool,
    otp_secret: Option<String>,
    last_auth_attempt: Option<NaiveDateTime>,
}

/// Outcome of a web login attempt.
#[derive(Debug)]
pub enum AuthOutcome {
    /// Succeeded — populate the session for this user.
    Success {
        id: i32,
        login: String,
        access_level: i32,
        pwd_hash: String,
    },
    /// OTP required — render the challenge page (HTTP 200).
    OtpChallenge,
    /// Failed — set `login_error_msg` and redirect.
    Failure { message: String },
}

pub async fn authenticate(pool: &sqlx::PgPool, login: &str, password: &str, otp: Option<i64>) -> AuthOutcome {
    // `UserHelper::find_user_by_login`: LOWER(login) = LOWER(?).
    let row = sqlx::query(
        "SELECT id, login, access_level, pwd_hash, salt, otp_enabled, otp_secret, last_auth_attempt
         FROM ttrss_users WHERE LOWER(login) = LOWER($1)",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return AuthOutcome::Failure {
            message: __(ERR_BAD_CREDENTIALS),
        };
    };
    let user = UserRow {
        id: r.try_get("id").unwrap_or(0),
        login: r.try_get("login").unwrap_or_default(),
        access_level: r.try_get("access_level").unwrap_or(0),
        pwd_hash: r.try_get("pwd_hash").unwrap_or_default(),
        salt: r.try_get("salt").unwrap_or_default(),
        otp_enabled: r.try_get("otp_enabled").unwrap_or(false),
        otp_secret: r.try_get("otp_secret").ok().flatten(),
        last_auth_attempt: r.try_get("last_auth_attempt").ok().flatten(),
    };

    if user.otp_enabled {
        match otp {
            Some(code) if code != 0 => {
                let secret = user.otp_secret.clone().unwrap_or_default();
                let ok = user::verify_password(&user.pwd_hash, password, &user.salt)
                    && user::check_otp(secret.as_bytes(), &code.to_string(), now_unix() as u64);
                if ok {
                    success(pool, &user, password).await
                } else {
                    AuthOutcome::Failure {
                        message: __(ERR_BAD_CREDENTIALS),
                    }
                }
            }
            _ => AuthOutcome::OtpChallenge,
        }
    } else {
        if let Some(laa) = user.last_auth_attempt
            && now_unix() - laa.and_utc().timestamp() < AUTH_MIN_INTERVAL {
                let _ = stamp_last_attempt(pool, user.id).await;
                return AuthOutcome::Failure {
                    message: __(ERR_THROTTLED),
                };
            }
        if user::verify_password(&user.pwd_hash, password, &user.salt) {
            success(pool, &user, password).await
        } else {
            let _ = stamp_last_attempt(pool, user.id).await;
            AuthOutcome::Failure {
                message: __(ERR_BAD_CREDENTIALS),
            }
        }
    }
}

/// `Auth_Internal::check_password` success side-effects + the
/// `UserHelper::authenticate` session setup: upgrade an out-of-date `pwd_hash`
/// to the current algorithm, stamp `last_login`, then return success (or
/// failure if the account is disabled).
async fn success(pool: &sqlx::PgPool, user: &UserRow, password: &str) -> AuthOutcome {
    if user.access_level == user::ACCESS_LEVEL_DISABLED {
        return AuthOutcome::Failure {
            message: __(ERR_BAD_CREDENTIALS),
        };
    }

    let mut pwd_hash = user.pwd_hash.clone();
    let stored_algo = user.pwd_hash.split_once(':').map(|(a, _)| a.to_string());
    if stored_algo.as_deref() != Some(crate::core::user::ALGO_SSHA512) {
        // `check_password`: "Upgrading password of user $login to SSHA-512"
        if let Some(new_hash) =
            crate::core::user::hash_password(password, &user.salt, crate::core::user::ALGO_SSHA512)
        {
            let _ = sqlx::query("UPDATE ttrss_users SET pwd_hash = $1 WHERE id = $2")
                .bind(&new_hash)
                .bind(user.id)
                .execute(pool)
                .await;
            pwd_hash = new_hash;
        }
    }

    let _ = sqlx::query("UPDATE ttrss_users SET last_login = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(pool)
        .await;

    AuthOutcome::Success {
        id: user.id,
        login: user.login.clone(),
        access_level: user.access_level,
        pwd_hash,
    }
}

async fn stamp_last_attempt(pool: &sqlx::PgPool, id: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ttrss_users SET last_auth_attempt = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// `Auth_Internal::check_password` (service = ""): verify `password` against
/// the stored hash (using the stored algorithm) and, on a match with an
/// out-of-date algorithm, upgrade the stored hash to SSHA-512. Returns true on
/// a match. Assumes `auth_internal` (the only auth module loaded out of the
/// gate).
pub async fn check_password(pool: &sqlx::PgPool, uid: i32, password: &str) -> bool {
    let row = sqlx::query("SELECT pwd_hash, salt FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(row) = row else {
        return false;
    };
    let pwd_hash: String = row.get(0);
    let salt: String = row.get(1);
    if !crate::core::user::verify_password(&pwd_hash, password, &salt) {
        return false;
    }
    if let Some((algo, _)) = pwd_hash.split_once(':')
        && algo != crate::core::user::ALGO_SSHA512
            && let Some(new_hash) =
                crate::core::user::hash_password(password, &salt, crate::core::user::ALGO_SSHA512)
            {
                let _ = sqlx::query("UPDATE ttrss_users SET pwd_hash = $1 WHERE id = $2")
                    .bind(&new_hash)
                    .bind(uid)
                    .execute(pool)
                    .await;
            }
    true
}

/// `Auth_Internal::change_password`: verify the old password, then set a fresh
/// salt + SSHA-512 hash. Returns the new hash on success (so the caller can
/// refresh the session) or `None` if the old password is wrong.
///
/// NOTE(port): the "password change notification" e-mail is omitted (PHP
/// sends it via the system MTA / `mail()`, which the Rust port does not
/// replicate). The `Config::FORBID_PASSWORD_CHANGES` guard lives in the
/// `changepassword` handler.
pub async fn change_password(
    pool: &sqlx::PgPool,
    uid: i32,
    old_password: &str,
    new_password: &str,
) -> Option<String> {
    if !check_password(pool, uid, old_password).await {
        return None;
    }
    let new_salt = crate::core::user::get_salt();
    let new_hash =
        crate::core::user::hash_password(new_password, &new_salt, crate::core::user::ALGO_SSHA512)?;
    let _ = sqlx::query("UPDATE ttrss_users SET salt = $1, pwd_hash = $2 WHERE id = $3")
        .bind(&new_salt)
        .bind(&new_hash)
        .bind(uid)
        .execute(pool)
        .await;
    Some(new_hash)
}

/// The session-setup half of `UserHelper::authenticate`'s success path: the
/// post-login `$_SESSION` contents plus a **fresh** session id (PHP calls
/// `session_regenerate_id(true)` here, so the pre-login id is never reused).
/// `bw_limit` / `safe_mode` are the already-parsed checkbox values.
pub fn build_login_session(
    id: i32,
    login: &str,
    access_level: i32,
    pwd_hash: &str,
    bw_limit: bool,
    safe_mode: bool,
    auth_module: &str,
) -> (String, Session) {
    let now = now_unix();
    let mut data = core_session::Session::new();
    data.set("uid", Value::Int(id as i64));
    data.set("last_login_update", Value::Int(now));
    data.set("ip_address", Value::Bytes(b"127.0.0.1".to_vec()));
    data.set("csrf_token", Value::Bytes(random_hex(16).into_bytes()));
    data.set("auth_module", Value::Bytes(auth_module.as_bytes().to_vec()));
    data.set("name", Value::Bytes(login.as_bytes().to_vec()));
    data.set("access_level", Value::Int(access_level as i64));
    data.set("pwd_hash", Value::Bytes(pwd_hash.as_bytes().to_vec()));
    data.set("ref_schema_version", Value::Int(SCHEMA_VERSION));
    data.set("bw_limit", Value::Bool(bw_limit));
    data.set("safe_mode", Value::Bool(safe_mode));
    (core_session::new_session_id(), data)
}

/// `Handler_Public::getProfiles` — the login page's Profile dropdown list
/// (the login form fetches `public.php?op=getprofiles&login=…` on login-field
/// change/focus/blur): `[{label, value}, …]` with the `Default profile` entry
/// (`value = 0`) always first. The login matches case-insensitively; without
/// a login an empty array is returned.
pub async fn get_profiles(db: &Db, login: &str) -> serde_json::Value {
    let login = crate::util::clean_str(login);
    if login.is_empty() {
        return serde_json::json!([]);
    }
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT p.title, p.id FROM ttrss_settings_profiles p \
         JOIN ttrss_users u ON u.id = p.owner_uid \
         WHERE LOWER(u.login) = LOWER($1) ORDER BY p.title ASC",
    )
    .bind(login)
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();
    let mut rv: Vec<serde_json::Value> =
        vec![serde_json::json!({ "label": __("Default profile"), "value": 0 })];
    for (title, id) in rows {
        rv.push(serde_json::json!({ "label": title, "value": id }));
    }
    serde_json::Value::Array(rv)
}
