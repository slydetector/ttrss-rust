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

use chrono::NaiveDateTime;
use rand::Rng;
use sqlx::Row;
use ttrss_core::config::SCHEMA_VERSION;
use ttrss_core::db::Db;
use ttrss_core::phpser::Value;
use ttrss_core::session::{self as core_session, Session};
use ttrss_core::user;
use crate::util::now_unix;

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
    Failure {
        message: String,
    },
}

pub async fn authenticate(
    db: &Db,
    login: &str,
    password: &str,
    otp: Option<i64>,
) -> AuthOutcome {
    let pool = db.pool();
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
            message: ERR_BAD_CREDENTIALS.into(),
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
                        message: ERR_BAD_CREDENTIALS.into(),
                    }
                }
            }
            _ => AuthOutcome::OtpChallenge,
        }
    } else {
        if let Some(laa) = user.last_auth_attempt {
            if now_unix() - laa.and_utc().timestamp() < AUTH_MIN_INTERVAL {
                let _ = stamp_last_attempt(pool, user.id).await;
                return AuthOutcome::Failure {
                    message: ERR_THROTTLED.into(),
                };
            }
        }
        if user::verify_password(&user.pwd_hash, password, &user.salt) {
            success(pool, &user, password).await
        } else {
            let _ = stamp_last_attempt(pool, user.id).await;
            AuthOutcome::Failure {
                message: ERR_BAD_CREDENTIALS.into(),
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
            message: ERR_BAD_CREDENTIALS.into(),
        };
    }

    let mut pwd_hash = user.pwd_hash.clone();
    let stored_algo = user.pwd_hash.split_once(':').map(|(a, _)| a.to_string());
    if stored_algo.as_deref() != Some(ttrss_core::user::ALGO_SSHA512) {
        // `check_password`: "Upgrading password of user $login to SSHA-512"
        if let Some(new_hash) = ttrss_core::user::hash_password(
            password,
            &user.salt,
            ttrss_core::user::ALGO_SSHA512,
        ) {
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
) -> (String, Session) {
    let now = now_unix();
    let mut data = core_session::Session::new();
    data.set("uid", Value::Int(id as i64));
    data.set("last_login_update", Value::Int(now));
    data.set("ip_address", Value::Bytes(b"127.0.0.1".to_vec()));
    data.set("csrf_token", Value::Bytes(random_hex(16).into_bytes()));
    data.set("auth_module", Value::Bytes(b"auth_internal".to_vec()));
    data.set("name", Value::Bytes(login.as_bytes().to_vec()));
    data.set("access_level", Value::Int(access_level as i64));
    data.set("pwd_hash", Value::Bytes(pwd_hash.as_bytes().to_vec()));
    data.set("ref_schema_version", Value::Int(SCHEMA_VERSION));
    data.set("bw_limit", Value::Bool(bw_limit));
    data.set("safe_mode", Value::Bool(safe_mode));
    (core_session::new_session_id(), data)
}

/// 16 random bytes as 32 lowercase hex chars (PHP-style csrf/session ids).
fn random_hex(n_bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n_bytes)
        .map(|_| format!("{:02x}", rng.gen_range(0u8..=255)))
        .collect()
}
