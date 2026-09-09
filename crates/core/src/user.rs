//! User password hashing, TOTP, and app-password verification — pure ports of
//! `UserHelper::hash_password`, the `auth_internal` plugin's `check_password` /
//! `check_app_password`, and `UserHelper::check_otp` (OTPPHP TOTP).

use sha1::{Digest as _, Sha1};
use sha2::{Sha256, Sha512};

// ---------------------------------------------------------------------------
// Password hashing
// ---------------------------------------------------------------------------

pub const ALGO_SSHA512: &str = "SSHA-512";
pub const ALGO_SSHA256: &str = "SSHA-256";
pub const ALGO_MODE2: &str = "MODE2";
pub const ALGO_SHA1X: &str = "SHA1X";
pub const ALGO_SHA1: &str = "SHA1";

/// `UserHelper::ACCESS_LEVEL_*` (`ttrss_users.access_level`).
pub const ACCESS_LEVEL_DISABLED: i32 = -2;
pub const ACCESS_LEVEL_READONLY: i32 = -1;
pub const ACCESS_LEVEL_USER: i32 = 0;
pub const ACCESS_LEVEL_POWERUSER: i32 = 5;
pub const ACCESS_LEVEL_ADMIN: i32 = 10;

/// `UserHelper::hash_password` — returns the full `ALGO:hash` string.
pub fn hash_password(pass: &str, salt: &str, algo: &str) -> Option<String> {
    let hex = match algo {
        ALGO_SHA1 => {
            let mut h = Sha1::new();
            h.update(pass.as_bytes());
            hex::encode(h.finalize())
        }
        ALGO_SHA1X => {
            let mut h = Sha1::new();
            h.update(salt.as_bytes());
            h.update(b":");
            h.update(pass.as_bytes());
            hex::encode(h.finalize())
        }
        ALGO_MODE2 | ALGO_SSHA256 => {
            let mut h = Sha256::new();
            h.update(salt.as_bytes());
            h.update(pass.as_bytes());
            hex::encode(h.finalize())
        }
        ALGO_SSHA512 => {
            let mut h = Sha512::new();
            h.update(salt.as_bytes());
            h.update(pass.as_bytes());
            hex::encode(h.finalize())
        }
        _ => return None,
    };
    Some(format!("{algo}:{hex}"))
}

/// `UserHelper::get_salt()`: 250 hex characters (`bin2hex` of 125 random
/// bytes). Used for the per-app-password salt embedded in `pwd_hash`.
pub fn get_salt() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 125];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut s = String::with_capacity(250);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Verify a stored user `pwd_hash` (`ALGO:hash`) against a candidate password
/// and salt — mirrors `auth_internal::check_password`.
pub fn verify_password(stored: &str, pass: &str, salt: &str) -> bool {
    let Some((algo, _raw)) = stored.split_once(':') else {
        return false;
    };
    match hash_password(pass, salt, algo) {
        Some(test) => test == stored,
        None => false,
    }
}

// ---------------------------------------------------------------------------
// App passwords (ALGO:HASH:SALT form, salt embedded)
// ---------------------------------------------------------------------------

/// Verify a stored app-password string (`ALGO:HASH:SALT`) against a candidate
/// password — mirrors `auth_internal::check_app_password`.
pub fn verify_app_password(stored: &str, pass: &str) -> bool {
    let mut parts = stored.splitn(3, ':');
    let (Some(algo), Some(raw_hash), Some(salt)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    match hash_password(pass, salt, algo) {
        Some(test) => {
            let (t_algo, t_hash) = test.split_once(':').unwrap_or(("", ""));
            t_algo == algo && t_hash == raw_hash
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// TOTP (OTPPHP TOTP: sha1, 30s, 6 digits)
// ---------------------------------------------------------------------------

/// Compute the 6-digit TOTP for the given secret bytes at unix time `t_secs`,
/// matching OTPHP `TOTP::now()` (no ±1 window).
pub fn totp_at(secret: &[u8], t_secs: u64) -> String {
    totp_with(secret, t_secs, 6)
}

/// General TOTP (RFC 4226/6238) with a configurable digit count.
pub fn totp_with(secret: &[u8], t_secs: u64, digits: u32) -> String {
    use hmac::{Hmac, Mac};
    let counter = t_secs / 30;
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&counter.to_be_bytes());

    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC key any size");
    mac.update(&buf);
    let digest = mac.finalize().into_bytes();

    let offset = (digest[19] & 0x0f) as usize;
    let code = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | (digest[offset + 3] as u32);
    let modulus = 10u32.pow(digits);
    format!("{:0width$}", code % modulus, width = digits as usize)
}

/// Verify a user-supplied OTP string against the current time (no window).
pub fn check_otp(secret: &[u8], otp: &str, t_secs: u64) -> bool {
    let expected = totp_at(secret, t_secs);
    // tt-rss uses `==` on the 6-digit strings; we match on the numeric value
    // to be tolerant of leading-zero input forms, then confirm equal length.
    let a: Option<u64> = otp.trim().parse().ok();
    let b: u64 = expected.parse().unwrap_or(0);
    match a {
        Some(x) if x == b => x.to_string().len() == expected.len() || otp.trim() == expected,
        _ => otp.trim() == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssha512_matches_php() {
        // sha512(salt + pass)
        let salt = "a1b2c3d4e5f6";
        let pass = "AppPass123";
        let h = hash_password(pass, salt, ALGO_SSHA512).unwrap();
        assert!(h.starts_with("SSHA-512:"));
        // verify round-trip
        assert!(verify_password(&h, pass, salt));
        assert!(!verify_password(&h, "wrong", salt));
    }

    #[test]
    fn all_algos() {
        let salt = "s";
        for algo in [
            ALGO_SHA1,
            ALGO_SHA1X,
            ALGO_MODE2,
            ALGO_SSHA256,
            ALGO_SSHA512,
        ] {
            let h = hash_password("p", salt, algo).unwrap();
            assert!(verify_password(&h, "p", salt), "algo {algo}");
        }
        assert!(hash_password("p", "s", "BOGUS").is_none());
    }

    #[test]
    fn app_password_format() {
        let salt = "a1b2c3d4e5f6";
        let pass = "AppPass123";
        let inner = hash_password(pass, salt, ALGO_SSHA512).unwrap();
        let stored = format!("{inner}:{salt}");
        assert!(verify_app_password(&stored, pass));
        assert!(!verify_app_password(&stored, "nope"));
        // malformed
        assert!(!verify_app_password("SSHA-512:abc", pass));
    }

    #[test]
    fn totp_rfc6238_vectors() {
        // RFC 6238 Appendix B, secret = ASCII "12345678901234567890"
        let secret = b"12345678901234567890";
        // T=59 (counter 1) -> 94287082 -> "287082"
        assert_eq!(totp_at(secret, 59), "287082");
        // T=1111111109 -> 07081804 -> "081804"
        assert_eq!(totp_at(secret, 1111111109), "081804");
    }

    #[test]
    fn check_otp_current_window() {
        let secret = b"JBSWY3DPEHPK3PXP";
        let now = 1_788_000_000u64;
        let code = totp_at(secret, now);
        assert!(check_otp(secret, &code, now));
        assert!(!check_otp(secret, "000000", now) || code == "000000");
    }
}
