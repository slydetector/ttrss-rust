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

use chrono::NaiveDateTime;
use sqlx::Row;
use ttrss_core::db::Db;
use ttrss_core::user;

/// `AUTH_MIN_INTERVAL` (seconds) — minimum gap between failed login attempts.
pub const AUTH_MIN_INTERVAL: i64 = 5;
/// `UserHelper::ACCESS_LEVEL_DISABLED`.
const ACCESS_LEVEL_DISABLED: i32 = -2;

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
                    success(&user)
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
            success(&user)
        } else {
            let _ = stamp_last_attempt(pool, user.id).await;
            AuthOutcome::Failure {
                message: ERR_BAD_CREDENTIALS.into(),
            }
        }
    }
}

fn success(user: &UserRow) -> AuthOutcome {
    if user.access_level == ACCESS_LEVEL_DISABLED {
        return AuthOutcome::Failure {
            message: ERR_BAD_CREDENTIALS.into(),
        };
    }
    AuthOutcome::Success {
        id: user.id,
        login: user.login.clone(),
        access_level: user.access_level,
        pwd_hash: user.pwd_hash.clone(),
    }
}

async fn stamp_last_attempt(pool: &sqlx::PgPool, id: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ttrss_users SET last_auth_attempt = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
