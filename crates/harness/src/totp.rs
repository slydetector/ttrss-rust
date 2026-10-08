//! TOTP for the corpus: tt-rss (OTPPHP) stores the user's `otp_secret` column
//! as raw ASCII and base32-encodes it on read, so the effective HMAC key is
//! simply the ASCII bytes of that column value. `{{totp:<secret>}}` computes a
//! live RFC 6238 code (SHA-1, 30s period, 6 digits) keyed on the ASCII bytes
//! of <secret> — exactly what tt-rss checks.

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub fn totp(secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    totp_at(secret, now)
}

/// TOTP code for a specific unix timestamp (seconds), keyed on the ASCII
/// bytes of `secret`.
pub fn totp_at(secret: &str, t_secs: u64) -> String {
    let key = secret.as_bytes();
    let counter = t_secs / 30;

    let mut buf = [0u8; 8];
    buf.copy_from_slice(&counter.to_be_bytes());

    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(&buf);
    let digest = mac.finalize().into_bytes();

    // dynamic truncation
    let offset = (digest[19] & 0x0f) as usize;
    let value = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | (digest[offset + 3] as u32);
    format!("{:06}", value % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B SHA-1 vectors; the RFC secret IS the ASCII string
    // "12345678901234567890", which matches our key semantics directly:
    //   T=59s (counter 1)          -> 94287082  -> "287082" (6 digits)
    //   T=1111111109 (c 0x23523EC) -> 07081804  -> "081804"
    #[test]
    fn rfc6238_vectors() {
        let secret = "12345678901234567890";
        assert_eq!(totp_at(secret, 59), "287082");
        assert_eq!(totp_at(secret, 1111111109), "081804");
    }

    #[test]
    fn six_digits() {
        let code = totp("JBSWY3DPEHPK3PXP");
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }
}
