//! Symmetric encryption of session data — a byte-for-byte port of tt-rss's
//! `Crypt::encrypt_string` / `decrypt_string`, which use the Sodium
//! `xchacha20poly1305_ietf` primitive.
//!
//! The wire format is a PHP-serialized 3-element associative array
//! (`algo`, `nonce`, `payload`), which the caller is responsible for
//! (de)serializing via [`crate::phpser`].

use aead::{Aead, KeyInit};
use anyhow::{anyhow, Result};
use chacha20poly1305::{XNonce, XChaCha20Poly1305};
use rand::RngCore;

pub const ALGO: &str = "xchacha20poly1305_ietf";

/// 256-bit key, matching tt-rss's 32-byte hex `TTRSS_ENCRYPTION_KEY`.
pub const KEY_LEN: usize = 32;
/// 192-bit extended nonce (SIPHASH), matching Sodium's xchacha20poly1305.
pub const NONCE_LEN: usize = 24;

/// Decoded envelope: the algorithm name plus the (base64-free) raw nonce and
/// ciphertext/payload.
#[derive(Debug, Clone)]
pub struct EncryptedData {
    pub algo: &'static str,
    pub nonce: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Decode a 64-hex-char `TTRSS_ENCRYPTION_KEY` into 32 bytes.
pub fn hex_key(hex: &str) -> Result<[u8; KEY_LEN]> {
    let b = hex::decode(hex.trim()).map_err(|e| anyhow!("invalid hex key: {e}"))?;
    if b.len() != KEY_LEN {
        return Err(anyhow!("key must be {KEY_LEN} bytes, got {}", b.len()));
    }
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&b);
    Ok(out)
}

/// Random 24-byte nonce.
pub fn random_nonce(rng: &mut impl RngCore) -> Vec<u8> {
    let mut n = vec![0u8; NONCE_LEN];
    rng.fill_bytes(&mut n);
    n
}

/// Encrypt `plaintext` with a fresh nonce, returning the envelope.
pub fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<EncryptedData> {
    let mut rng = rand::thread_rng();
    let nonce = random_nonce(&mut rng);
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| anyhow!("invalid key: {e}"))?;
    let payload = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|e| anyhow!("encrypt failed: {e}"))?;
    Ok(EncryptedData {
        algo: ALGO,
        nonce,
        payload,
    })
}

/// Decrypt an envelope's payload back to plaintext.
pub fn decrypt(key: &[u8; KEY_LEN], nonce: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| anyhow!("invalid key: {e}"))?;
    let out = cipher
        .decrypt(XNonce::from_slice(nonce), payload)
        .map_err(|e| anyhow!("decrypt failed: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn hex_key_parses() {
        let k = hex_key(KEY).unwrap();
        assert_eq!(k.len(), 32);
        assert!(hex_key("zz").is_err()); // not hex
        assert!(hex_key("0011223344556677").is_err()); // valid hex, wrong length
    }

    #[test]
    fn roundtrip() {
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"session plaintext").unwrap();
        assert_eq!(enc.algo, ALGO);
        assert_eq!(enc.nonce.len(), NONCE_LEN);
        let dec = decrypt(&key, &enc.nonce, &enc.payload).unwrap();
        assert_eq!(dec, b"session plaintext");
    }

    #[test]
    fn bad_key_fails() {
        let key = hex_key(KEY).unwrap();
        let mut other = key;
        other[0] ^= 0xff;
        let enc = encrypt(&key, b"secret").unwrap();
        assert!(decrypt(&other, &enc.nonce, &enc.payload).is_err());
    }

    #[test]
    fn matches_raw_sodium_api() {
        // Our wrappers must agree with the raw chacha20poly1305 API (the same
        // xchacha20poly1305_ietf primitive PHP sodium uses).
        let key = hex_key(KEY).unwrap();
        let nonce = random_nonce(&mut rand::thread_rng());
        let plain: &[u8] = b"The quick brown fox jumps over the lazy dog";
        let raw = XChaCha20Poly1305::new_from_slice(&key)
            .unwrap()
            .encrypt(XNonce::from_slice(&nonce), plain)
            .unwrap();
        // decrypt() recovers ciphertext produced directly by the raw crate.
        assert_eq!(decrypt(&key, &nonce, &raw).unwrap(), plain);
        // And encrypt() (fresh nonce) round-trips through decrypt().
        let enc = encrypt(&key, plain).unwrap();
        assert_eq!(decrypt(&key, &enc.nonce, &enc.payload).unwrap(), plain);
    }
}
