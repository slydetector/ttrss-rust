//! tt-rss DB-backed sessions.
//!
//! A session row (`ttrss_sessions.data`) holds, when `TTRSS_ENCRYPTION_KEY` is
//! set:
//!
//!   base64( php_serialize( {algo, nonce, payload} ) )
//!
//! where `payload` is the XChaCha20-Poly1305-IETF ciphertext of the session
//! *data*, and the session *data* is PHP's `php` serialize-handler format:
//!
//!   key1|serialize(v1) key2|serialize(v2) ...        (concatenated, no sep)
//!
//! This module parses/renders both layers and offers a typed [`Session`] view.

use crate::phpser::{self, Key, Value};
use anyhow::{anyhow, Result};
use base64::Engine as _;

const DELIMITER: u8 = b'|';

/// An ordered tt-rss session (the decoded `$_SESSION`).
#[derive(Debug, Clone, Default)]
pub struct Session {
    items: Vec<(String, Value)>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from decoded items (the inverse of [`Session::items`]).
    pub fn from_items(items: Vec<(String, Value)>) -> Self {
        Self { items }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.as_int())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    /// Insert or update a value, preserving existing order on update.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(slot) = self.items.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.items.push((key.to_string(), value));
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.items.retain(|(k, _)| k != key);
    }

    pub fn items(&self) -> &[(String, Value)] {
        &self.items
    }

    // -- tt-rss specific accessors -------------------------------------------
    pub fn uid(&self) -> Option<i64> {
        self.get_int("uid")
    }
    pub fn is_logged_in(&self) -> bool {
        self.get_int("uid").is_some_and(|u| u > 0)
    }
    pub fn csrf_token(&self) -> Option<&str> {
        self.get_str("csrf_token")
    }
    pub fn name(&self) -> Option<&str> {
        self.get_str("name")
    }
    pub fn access_level(&self) -> Option<i64> {
        self.get_int("access_level")
    }
    pub fn profile(&self) -> Option<i64> {
        self.get("profile").and_then(|v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        })
    }
    pub fn safe_mode(&self) -> bool {
        self.get_bool("safe_mode").unwrap_or(false)
    }
    pub fn pwd_hash(&self) -> Option<&str> {
        self.get_str("pwd_hash")
    }
}

// ---------------------------------------------------------------------------
// Session *data* format: key|serialize(value) concatenation
// ---------------------------------------------------------------------------

/// Encode the session data (PHP `php` handler format).
///
/// Returns an error if a key contains the `|` delimiter (PHP rejects such
/// keys — "Data contains invalid key").
pub fn encode_data(items: &[(String, Value)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (key, value) in items {
        if key.as_bytes().contains(&DELIMITER) {
            return Err(anyhow!("invalid session key (contains '|'): {key:?}"));
        }
        out.extend_from_slice(key.as_bytes());
        out.push(DELIMITER);
        phpser::serialize_into(&mut out, value);
    }
    Ok(out)
}

/// Decode the session data format into ordered (key, value) pairs.
pub fn decode_data(bytes: &[u8]) -> Result<Vec<(String, Value)>> {
    let mut items = Vec::new();
    let mut p = 0;
    while p < bytes.len() {
        // find the delimiter that ends the key
        let q = bytes[p..]
            .iter()
            .position(|&b| b == DELIMITER)
            .ok_or_else(|| anyhow!("session data: missing '|' delimiter at byte {p}"))?
            + p;
        let key = String::from_utf8_lossy(&bytes[p..q]).into_owned();
        let mut vp = q + 1;
        let (value, np) = phpser::parse_value_at(bytes, vp)?;
        vp = np;
        p = vp;
        items.push((key, value));
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Envelope + base64 (the stored `ttrss_sessions.data` form)
// ---------------------------------------------------------------------------

use crate::crypt;

/// Encrypt `data` into the envelope form: a PHP-serialized
/// `a:3:{s:4:"algo";\u2026s:5:"nonce";\u2026s:7:"payload";\u2026}` object (PHP
/// `serialize(Crypt::encrypt_string($data))`).
pub fn encrypt_envelope(data: &[u8], key: &[u8; crypt::KEY_LEN]) -> Result<Vec<u8>> {
    let enc = crypt::encrypt(key, data)?;
    let envelope = Value::Array(vec![
        (Key::Bytes(b"algo".to_vec()), Value::Bytes(enc.algo.as_bytes().to_vec())),
        (Key::Bytes(b"nonce".to_vec()), Value::Bytes(enc.nonce)),
        (Key::Bytes(b"payload".to_vec()), Value::Bytes(enc.payload)),
    ]);
    Ok(phpser::serialize(&envelope))
}

/// Decrypt an envelope (the PHP-serialized `a:3:{algo,nonce,payload}` object
/// from [`encrypt_envelope`]) back to the original data (PHP
/// `Crypt::decrypt_string(unserialize($data))`).
pub fn decrypt_envelope(inner: &[u8], key: &[u8; crypt::KEY_LEN]) -> Result<Vec<u8>> {
    let envelope = phpser::deserialize(inner)?;
    let algo = envelope
        .get("algo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("session envelope missing 'algo'"))?;
    if algo != crypt::ALGO {
        return Err(anyhow!("unsupported session algo: {algo}"));
    }
    let nonce = envelope
        .get("nonce")
        .and_then(|v| v.as_bytes())
        .ok_or_else(|| anyhow!("session envelope missing 'nonce'"))?
        .to_vec();
    let payload = envelope
        .get("payload")
        .and_then(|v| v.as_bytes())
        .ok_or_else(|| anyhow!("session envelope missing 'payload'"))?
        .to_vec();
    crypt::decrypt(key, &nonce, &payload)
}

/// Generate a new PHP-style session id: 16 random bytes as 32 lowercase hex
/// chars (PHP defaults `session.sid_length=32`, `sid_bits_per_character=4`).
pub fn new_session_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Build the stored (base64) form of a session from its data, encrypting with
/// `key`. `key == None` stores unencrypted (no `TTRSS_ENCRYPTION_KEY`).
pub fn to_stored(items: &[(String, Value)], key: Option<&[u8; crypt::KEY_LEN]>) -> Result<String> {
    let data = encode_data(items)?;
    let inner: Vec<u8> = match key {
        None => data,
        Some(k) => encrypt_envelope(&data, k)?,
    };
    Ok(base64::engine::general_purpose::STANDARD.encode(inner))
}

/// Parse a stored `ttrss_sessions.data` value into a [`Session`].
pub fn from_stored(stored: &str, key: Option<&[u8; crypt::KEY_LEN]>) -> Result<Session> {
    if stored.is_empty() {
        return Ok(Session::new());
    }
    let inner = base64::engine::general_purpose::STANDARD
        .decode(stored.as_bytes())
        .map_err(|e| anyhow!("session data is not valid base64: {e}"))?;

    let data: Vec<u8> = match key {
        None => inner,
        Some(k) => decrypt_envelope(&inner, k)?,
    };

    let items = decode_data(&data)?;
    Ok(Session { items })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        let mut s = Session::new();
        s.set("uid", Value::Int(1));
        s.set("last_login_update", Value::Int(1788473616));
        s.set("ip_address", Value::Bytes(b"172.18.0.1".to_vec()));
        s.set("csrf_token", Value::Bytes(b"4e2e86d00a426322856d2722d1d98e4e".to_vec()));
        s.set("auth_module", Value::Bytes(b"auth_internal".to_vec()));
        s.set("name", Value::Bytes(b"admin".to_vec()));
        s.set("access_level", Value::Int(10));
        s.set(
            "pwd_hash",
            Value::Bytes(b"SSHA-512:1329311ae88c2cacca488e921ee12c5156".to_vec()),
        );
        s.set("plugin_blacklist", Value::Array(vec![]));
        s.set("hasSandbox", Value::Bool(false));
        s
    }

    #[test]
    fn data_format_matches_php() {
        let s = sample_session();
        let data = encode_data(s.items()).unwrap();
        let d = String::from_utf8_lossy(&data);
        // Must start exactly like the real PHP output.
        assert!(d.starts_with("uid|i:1;"), "got: {d}");
        assert!(d.contains("name|s:5:\"admin\";"));
        assert!(d.contains("access_level|i:10;"));
        assert!(d.contains("plugin_blacklist|a:0:{}"));
        assert!(d.contains("hasSandbox|b:0;"));
    }

    #[test]
    fn roundtrip_unencrypted() {
        let s = sample_session();
        let stored = to_stored(s.items(), None).unwrap();
        let back = from_stored(&stored, None).unwrap();
        assert_eq!(back.uid(), Some(1));
        assert_eq!(back.csrf_token(), Some("4e2e86d00a426322856d2722d1d98e4e"));
        assert_eq!(back.access_level(), Some(10));
        assert_eq!(back.name(), Some("admin"));
        assert!(!back.safe_mode());
    }

    #[test]
    fn roundtrip_encrypted() {
        let key = crypt::hex_key("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08").unwrap();
        let s = sample_session();
        let stored = to_stored(s.items(), Some(&key)).unwrap();
        // stored is base64 of a:3:{s:4:"algo";...} (contains raw nonce/payload bytes)
        let decoded_inner = base64::engine::general_purpose::STANDARD.decode(&stored).unwrap();
        assert!(decoded_inner.starts_with(b"a:3:{s:4:\"algo\";"));
        let back = from_stored(&stored, Some(&key)).unwrap();
        assert_eq!(back.uid(), Some(1));
        assert_eq!(back.name(), Some("admin"));
        assert_eq!(back.get_int("last_login_update"), Some(1788473616));
    }

    #[test]
    fn empty_stored_is_empty_session() {
        let s = from_stored("", None).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn real_oracle_row_roundtrip() {
        // Real decrypted plaintext captured from the oracle.
        let plaintext = "uid|i:1;last_login_update|i:1788473616;ip_address|s:10:\"172.18.0.1\";csrf_token|s:32:\"4e2e86d00a426322856d2722d1d98e4e\";auth_module|s:13:\"auth_internal\";name|s:5:\"admin\";access_level|i:10;pwd_hash|s:137:\"SSHA-512:1329311ae88c2cacca488e921ee12c5156b53e7e9670bd5bd2e18074f39b047d9fd545e17d3b4bc902029018dc0505e70b3be00a1adcd4790e2092ad0f604480\";plugin_blacklist|a:0:{}hasSandbox|b:0;";
        let items = decode_data(plaintext.as_bytes()).unwrap();
        let re = encode_data(&items).unwrap();
        // Byte-for-byte stable (round-trips the exact PHP output).
        assert_eq!(re, plaintext.as_bytes());
    }
}
