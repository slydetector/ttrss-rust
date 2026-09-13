//! Symmetric encryption of session data — a byte-for-byte port of tt-rss's
//! `Crypt::encrypt_string` / `decrypt_string`, which use the Sodium
//! `xchacha20poly1305_ietf` primitive.
//!
//! The wire format is a PHP-serialized 3-element associative array
//! (`algo`, `nonce`, `payload`), which the caller is responsible for
//! (de)serializing via [`crate::core::phpser`].

use aead::{Aead, KeyInit};
use anyhow::{anyhow, Result};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
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
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|e| anyhow!("invalid key: {e}"))?;
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
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|e| anyhow!("invalid key: {e}"))?;
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

    // ── Ported 1:1 from tt-rss/tests_functional/CryptTest.php ──────────────
    // The PHP `Crypt` class also owns the config lookup, base64 codec, and the
    // `algo` whitelist check; in this port those live in the *caller* (the
    // session/DB layer) and are covered there. The cases below exercise the
    // primitive itself (encrypt/decrypt) the same way the PHP suite does.
    const KEY2: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a09";

    #[test]
    fn encrypt_returns_valid_structure() {
        // test_encrypt_string_returns_valid_structure
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"hello world").unwrap();
        assert_eq!(enc.algo, ALGO);
        assert_eq!(enc.nonce.len(), NONCE_LEN);
        assert!(!enc.payload.is_empty());
    }

    #[test]
    fn encrypt_empty_string() {
        // test_encrypt_string_with_empty_string
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"").unwrap();
        assert_eq!(enc.algo, ALGO);
        assert!(!enc.payload.is_empty()); // poly1305 tag is always present
        assert_eq!(decrypt(&key, &enc.nonce, &enc.payload).unwrap(), b"");
    }

    #[test]
    fn encrypt_unicode_roundtrip() {
        // test_encrypt_string_with_unicode + test_decrypt_string_roundtrip_with_unicode
        let key = hex_key(KEY).unwrap();
        let original = "こんにちは世界 🌍 Привет мир 你好世界".as_bytes();
        let enc = encrypt(&key, original).unwrap();
        assert!(!enc.payload.is_empty());
        assert_eq!(decrypt(&key, &enc.nonce, &enc.payload).unwrap(), original);
    }

    #[test]
    fn encrypt_large_payload_roundtrip() {
        // test_encrypt_string_with_large_payload + ..._roundtrip_with_large_payload
        let key = hex_key(KEY).unwrap();
        let large = vec![b'A'; 100_000];
        let enc = encrypt(&key, &large).unwrap();
        assert!(!enc.payload.is_empty());
        assert_eq!(decrypt(&key, &enc.nonce, &enc.payload).unwrap(), large);
    }

    #[test]
    fn decrypt_roundtrip() {
        // test_decrypt_string_roundtrip
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"hello world").unwrap();
        assert_eq!(
            decrypt(&key, &enc.nonce, &enc.payload).unwrap(),
            b"hello world"
        );
    }

    #[test]
    fn decrypt_fails_with_tampered_payload() {
        // test_decrypt_string_fails_with_tampered_payload
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"secret").unwrap();
        let mut payload = enc.payload.clone();
        payload[0] ^= 0xff;
        assert!(decrypt(&key, &enc.nonce, &payload).is_err());
    }

    #[test]
    fn decrypt_fails_with_tampered_nonce() {
        // test_decrypt_string_fails_with_tampered_nonce
        let key = hex_key(KEY).unwrap();
        let enc = encrypt(&key, b"secret").unwrap();
        let mut nonce = enc.nonce.clone();
        nonce[0] ^= 0xff;
        assert!(decrypt(&key, &nonce, &enc.payload).is_err());
    }

    #[test]
    fn decrypt_fails_with_wrong_key() {
        // (key-analogue of the PHP roundtrip-with-different-key case)
        let key = hex_key(KEY).unwrap();
        let other = hex_key(KEY2).unwrap();
        let enc = encrypt(&key, b"secret").unwrap();
        assert!(decrypt(&other, &enc.nonce, &enc.payload).is_err());
    }

    #[test]
    fn different_keys_same_nonce_differ() {
        // test_encrypt_string_with_different_keys_produces_different_ciphertext
        // (a fixed nonce isolates the key; the primitive is deterministic).
        let key = hex_key(KEY).unwrap();
        let other = hex_key(KEY2).unwrap();
        let nonce = [42u8; NONCE_LEN];
        let data: &[u8] = b"same data";
        let c1 = XChaCha20Poly1305::new_from_slice(&key)
            .unwrap()
            .encrypt(XNonce::from_slice(&nonce), data)
            .unwrap();
        let c2 = XChaCha20Poly1305::new_from_slice(&other)
            .unwrap()
            .encrypt(XNonce::from_slice(&nonce), data)
            .unwrap();
        assert_ne!(c1, c2);
    }

    #[test]
    fn encrypt_produces_unique_nonces() {
        // test_generate_key_produces_unique_keys analogue for the nonce: two
        // encrypt() calls use distinct nonces, so ciphertexts differ.
        let key = hex_key(KEY).unwrap();
        let a = encrypt(&key, b"same").unwrap();
        let b = encrypt(&key, b"same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.payload, b.payload);
    }

    // ── N/A / REVISIT (REVISIT) ──────────────────────────────────────────────
    // These CryptTest.php cases exercise the PHP `Crypt` class's config lookup,
    // base64 codec, and `algo` whitelist — behavior this port keeps in the
    // *caller* (session/DB layer). Tracked in docs/plan.md (test-parity,
    // core::crypt row). Revisit when the Rust session/DB layer owns the codec:
    //   test_generate_key_returns_valid_length / _produces_unique_keys
    //   (key generation is config-provided here, not a Crypt::generate_key)
    //   test_encrypt_string_requires_encryption_key
    //   test_decrypt_string_requires_encryption_key
    //   test_decrypt_string_with_wrong_algo (algo whitelist check)
    //   test_decrypt_string_with_invalid_payload (base64 codec)
}
