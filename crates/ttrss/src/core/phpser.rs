//! PHP `serialize()` / `unserialize()` for the value shapes tt-rss stores in
//! session rows.
//!
//! A PHP string is a *byte string* (not necessarily valid UTF-8), so this
//! models it as [`Value::Bytes`] and all (de)serialization operates on raw
//! bytes. The serializer matches PHP's `php_var_serialize` output
//! byte-for-byte for the relevant subset:
//!
//!   value  := 'N;'                       null
//!          | 'b:' ('0'|'1') ';'          bool
//!          | 'i:' int ';'                int
//!          | 'd:' float ';'              float
//!          | 's:' len ':' <len bytes> ';' string (byte string)
//!          | 'a:' count ':' '{' (key value)* '}'   array
//!   key    := 's:' len ':' <len bytes> ';'  (assoc keys)
//!          | int ';'                      (list-array keys)
//!
//! PHP `serialize()` performs **no escaping** of string bytes; the length
//! prefix makes each value self-delimiting.
//!
//! The session *data* format (PHP's `php` session serialize handler,
//! ext/session/session.c `PS_SERIALIZER_(EN|DE)CODE_FUNC(php)`) is a
//! concatenation of `key|<serialized value>` pairs with **no** separator; the
//! keys may not contain `|`. See [`crate::core::session`] for that layer.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A PHP string (byte string).
    Bytes(Vec<u8>),
    /// Insertion-ordered array (PHP arrays preserve insertion order).
    Array(Vec<(Key, Value)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Int(i64),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bytes(b) => std::str::from_utf8(b)
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    /// UTF-8 view of a byte-string value (None if not valid UTF-8).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }
    /// UTF-8 (lossy) string view for logging / convenience.
    pub fn to_string_lossy(&self) -> String {
        match self {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            Value::Float(f) => f.to_string(),
            Value::Array(_) => String::from_utf8_lossy(&serialize(self)).into_owned(),
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[(Key, Value)]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
    /// Look up an assoc-array key by name.
    pub fn get(&self, key: &str) -> Option<&Value> {
        if let Some(items) = self.as_array() {
            for (k, v) in items {
                if let Key::Bytes(ks) = k {
                    if ks == key.as_bytes() {
                        return Some(v);
                    }
                }
            }
        }
        None
    }
}

impl Key {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Key::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

pub fn serialize(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_into(&mut out, v);
    out
}

/// Serialize a single value into a byte buffer.
pub fn serialize_into(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.extend_from_slice(b"N;"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"b:1;" } else { b"b:0;" }),
        Value::Int(i) => out.extend_from_slice(format!("i:{i};").as_bytes()),
        Value::Float(f) => out.extend_from_slice(format!("d:{f};").as_bytes()),
        Value::Bytes(b) => {
            out.extend_from_slice(format!("s:{}:", b.len()).as_bytes());
            out.push(b'"');
            out.extend_from_slice(b);
            out.extend_from_slice(b"\";");
        }
        Value::Array(items) => {
            out.extend_from_slice(format!("a:{}:{{", items.len()).as_bytes());
            for (k, val) in items {
                write_key(out, k);
                serialize_into(out, val);
            }
            out.push(b'}');
        }
    }
}

fn write_key(out: &mut Vec<u8>, k: &Key) {
    match k {
        Key::Int(i) => out.extend_from_slice(format!("{i};").as_bytes()),
        Key::Bytes(b) => {
            out.extend_from_slice(format!("s:{}:", b.len()).as_bytes());
            out.push(b'"');
            out.extend_from_slice(b);
            out.extend_from_slice(b"\";");
        }
    }
}

// ---------------------------------------------------------------------------
// Deserialization
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ParseError {
    pos: usize,
    msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "phpser: {} at byte {}", self.msg, self.pos)
    }
}

impl std::error::Error for ParseError {}

/// Parse a single PHP-serialized value starting at byte `pos`, returning the
/// value and the offset just past it.
pub fn parse_value_at(s: &[u8], pos: usize) -> Result<(Value, usize), ParseError> {
    if pos >= s.len() {
        return Err(err(pos, "unexpected end of input"));
    }
    let tag = s[pos];
    let mut p = pos + 1;
    let v = match tag {
        b'N' => {
            expect_char(s, &mut p, b';')?;
            Value::Null
        }
        b'b' => {
            expect_char(s, &mut p, b':')?;
            let c = *s.get(p).ok_or_else(|| err(p, "unexpected end"))?;
            p += 1;
            let val = match c {
                b'1' => true,
                b'0' => false,
                _ => return Err(err(p - 1, "bad bool")),
            };
            expect_char(s, &mut p, b';')?;
            Value::Bool(val)
        }
        b'i' => {
            expect_char(s, &mut p, b':')?;
            let (v, np) = parse_int(s, p)?;
            let mut p2 = np;
            expect_char(s, &mut p2, b';')?;
            p = p2;
            Value::Int(v)
        }
        b'd' => {
            expect_char(s, &mut p, b':')?;
            let (v, np) = parse_float(s, p)?;
            let mut p2 = np;
            expect_char(s, &mut p2, b';')?;
            p = p2;
            Value::Float(v)
        }
        b's' => {
            expect_char(s, &mut p, b':')?;
            let (len, np) = parse_uint(s, p)?;
            let mut p2 = np;
            expect_char(s, &mut p2, b':')?;
            expect_char(s, &mut p2, b'"')?; // opening quote
            let start = p2;
            let end = start
                .checked_add(len)
                .ok_or_else(|| err(start, "string length overflow"))?;
            if end > s.len() {
                return Err(err(start, "string runs past end"));
            }
            let bytes = s[start..end].to_vec();
            p = end;
            expect_char(s, &mut p, b'"')?; // closing quote
            expect_char(s, &mut p, b';')?;
            Value::Bytes(bytes)
        }
        b'a' => {
            expect_char(s, &mut p, b':')?;
            let (count, np) = parse_uint(s, p)?;
            let mut p2 = np;
            expect_char(s, &mut p2, b':')?;
            expect_char(s, &mut p2, b'{')?;
            p = p2;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let (k, kp) = parse_key(s, p)?;
                let (val, vp) = parse_value_at(s, kp)?;
                p = vp;
                items.push((k, val));
            }
            expect_char(s, &mut p, b'}')?;
            Value::Array(items)
        }
        other => return Err(err(pos, format!("unknown value tag {other:?}"))),
    };
    Ok((v, p))
}

/// Parse a complete single-value serialization (must consume all of `s`).
pub fn deserialize(s: &[u8]) -> Result<Value, ParseError> {
    let (v, pos) = parse_value_at(s, 0)?;
    if pos != s.len() {
        return Err(err(
            pos,
            format!("trailing data ({} bytes left)", s.len() - pos),
        ));
    }
    Ok(v)
}

fn parse_key(s: &[u8], pos: usize) -> Result<(Key, usize), ParseError> {
    if pos >= s.len() {
        return Err(err(pos, "unexpected end of input in key"));
    }
    if s[pos] == b's' {
        let mut p = pos + 1;
        expect_char(s, &mut p, b':')?;
        let (len, np) = parse_uint(s, p)?;
        let mut p2 = np;
        expect_char(s, &mut p2, b':')?;
        expect_char(s, &mut p2, b'"')?; // opening quote
        let start = p2;
        let end = start
            .checked_add(len)
            .ok_or_else(|| err(start, "key length overflow"))?;
        if end > s.len() {
            return Err(err(start, "key string runs past end"));
        }
        let bytes = s[start..end].to_vec();
        p = end;
        expect_char(s, &mut p, b'"')?; // closing quote
        expect_char(s, &mut p, b';')?;
        Ok((Key::Bytes(bytes), p))
    } else {
        let (v, np) = parse_int(s, pos)?;
        let mut p2 = np;
        expect_char(s, &mut p2, b';')?;
        Ok((Key::Int(v), p2))
    }
}

fn parse_int(s: &[u8], mut pos: usize) -> Result<(i64, usize), ParseError> {
    let start = pos;
    let mut neg = false;
    if pos < s.len() && (s[pos] == b'-' || s[pos] == b'+') {
        neg = s[pos] == b'-';
        pos += 1;
    }
    let mut val: i64 = 0;
    let mut seen = false;
    while pos < s.len() && s[pos].is_ascii_digit() {
        val = val
            .checked_mul(10)
            .and_then(|v| v.checked_add((s[pos] - b'0') as i64))
            .ok_or_else(|| err(pos, "int overflow"))?;
        pos += 1;
        seen = true;
    }
    if !seen {
        return Err(err(start, "expected integer digits"));
    }
    Ok((if neg { -val } else { val }, pos))
}

fn parse_uint(s: &[u8], pos: usize) -> Result<(usize, usize), ParseError> {
    let (v, np) = parse_int(s, pos)?;
    if v < 0 {
        return Err(err(pos, "negative count/length"));
    }
    Ok((v as usize, np))
}

fn parse_float(s: &[u8], mut pos: usize) -> Result<(f64, usize), ParseError> {
    let start = pos;
    if pos < s.len() && (s[pos] == b'-' || s[pos] == b'+') {
        pos += 1;
    }
    while pos < s.len()
        && (s[pos].is_ascii_digit() || s[pos] == b'.' || s[pos] == b'e' || s[pos] == b'E')
    {
        pos += 1;
    }
    let slice = std::str::from_utf8(&s[start..pos]).map_err(|_| err(start, "bad float bytes"))?;
    slice
        .parse::<f64>()
        .map(|f| (f, pos))
        .map_err(|_| err(start, "bad float"))
}

fn expect_char(s: &[u8], pos: &mut usize, expected: u8) -> Result<(), ParseError> {
    match s.get(*pos) {
        Some(&c) if c == expected => {
            *pos += 1;
            Ok(())
        }
        Some(&c) => Err(err(*pos, format!("expected {expected:?}, found {c:?}"))),
        None => Err(err(*pos, "unexpected end")),
    }
}

fn err(pos: usize, msg: impl Into<String>) -> ParseError {
    ParseError {
        pos,
        msg: msg.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &[u8]) -> Value {
        Value::Bytes(b.to_vec())
    }

    #[test]
    fn roundtrip_scalars() {
        let cases = vec![
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(42),
            Value::Int(-7),
            Value::Float(1.5),
            s(b"hello"),
            s(b""),
            s(b"a|b;c=d"),             // no escaping in PHP serialize
            s(&[0u8, 1, 2, 250, 255]), // raw binary
        ];
        for v in cases {
            let ser = serialize(&v);
            let back = deserialize(&ser).unwrap();
            assert_eq!(back, v, "failed for {v:?}");
        }
    }

    #[test]
    fn exact_scalar_output() {
        assert_eq!(serialize(&Value::Int(1)), b"i:1;");
        assert_eq!(serialize(&s(b"admin")), b"s:5:\"admin\";");
        assert_eq!(serialize(&Value::Bool(false)), b"b:0;");
        assert_eq!(serialize(&Value::Array(vec![])), b"a:0:{}");
        assert_eq!(serialize(&Value::Null), b"N;");
    }

    #[test]
    fn roundtrip_assoc() {
        let v = Value::Array(vec![
            (Key::Bytes(b"uid".to_vec()), Value::Int(1)),
            (Key::Bytes(b"name".to_vec()), s(b"admin")),
            (Key::Bytes(b"empty".to_vec()), Value::Array(vec![])),
            (Key::Bytes(b"flag".to_vec()), Value::Bool(false)),
        ]);
        let ser = serialize(&v);
        let back = deserialize(&ser).unwrap();
        assert_eq!(back, v);
        assert_eq!(back.get("uid").and_then(|x| x.as_int()), Some(1));
        assert_eq!(back.get("name").and_then(|x| x.as_str()), Some("admin"));
    }

    #[test]
    fn envelope_shape() {
        // The encrypted-session envelope is a:3:{algo, nonce, payload}.
        let nonce = vec![0x5du8, 0x1a, 0xe6, 0x57];
        let payload = vec![0x5au8, 0x7a, 0x72];
        let env = Value::Array(vec![
            (Key::Bytes(b"algo".to_vec()), s(b"xchacha20poly1305_ietf")),
            (Key::Bytes(b"nonce".to_vec()), Value::Bytes(nonce.clone())),
            (
                Key::Bytes(b"payload".to_vec()),
                Value::Bytes(payload.clone()),
            ),
        ]);
        let ser = serialize(&env);
        let back = deserialize(&ser).unwrap();
        assert_eq!(
            back.get("nonce").and_then(|v| v.as_bytes()),
            Some(nonce.as_slice())
        );
        assert_eq!(
            back.get("payload").and_then(|v| v.as_bytes()),
            Some(payload.as_slice())
        );
    }
}
