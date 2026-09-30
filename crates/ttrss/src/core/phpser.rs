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
//!          | 'd:' float ';'              float (`zend_gcvt` form — see
//!                                            [`php_float_str`]; non-finite
//!                                            values use PHP's `NAN`/`INF`/
//!                                            `-INF` tokens)
//!          | 's:' len ':' <len bytes> ';' string (byte string)
//!          | 'a:' count ':' '{' (key value)* '}'   array
//!   key    := 'i:' int ';'                 (integer keys — PHP's `serialize()`
//!                                            writes `i:` for *every* integer key,
//!                                            list positions included; verified
//!                                            against PHP 8.5.10)
//!          | int ';'                      (legacy bare form: what this port
//!                                            wrote before the fix — the parser
//!                                            still accepts it; PHP never emits
//!                                            it)
//!          | 's:' len ':' <len bytes> ';'  (assoc keys)
//!
//! PHP `serialize()` performs **no escaping** of string bytes; the length
//! prefix makes each value self-delimiting.
//!
//! The session *data* format (PHP's `php` session serialize handler,
//! ext/session/session.c `PS_SERIALIZER_(EN|DE)CODE_FUNC(php)`) is a
//! concatenation of `key|<serialized value>` pairs with **no** separator; the
//! keys may not contain `|`. See [`crate::core::session`] for that layer.

//! **Coverage notes** (verified against the PHP 8.5.10 sources —
//! `ext/standard/var.c`, `ext/standard/var_unserializer.re`,
//! `Zend/zend_strtod.c` `zend_gcvt`):
//!
//! - out of subset: `r:`/`R:` (references), `C:`/`O:` (objects) and the
//!   `N;` placeholders PHP writes for self-recursive arrays — tt-rss
//!   session/storage data never contains objects, references, or cycles;
//! - the parser does **not** accept the deprecated `S:` string form
//!   (PHP keeps reading it with a deprecation warning; the 8.5 serializer
//!   never emits it);
//! - the parser is a deliberate *superset* on malformed input: it also
//!   accepts legacy bare-integer keys and hex-float literals that PHP
//!   rejects, and it is *stricter* than PHP on trailing data (PHP warns
//!   and returns the value; this parser errors);
//! - decode semantics diverge from PHP only on hand-crafted data: duplicate
//!   keys (PHP nulls later duplicates; this keeps them) and numeric-string
//!   keys (PHP coerces them to integer keys; this keeps them strings).

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
                if let Key::Bytes(ks) = k
                    && ks == key.as_bytes() {
                        return Some(v);
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
        Value::Float(f) => {
            out.extend_from_slice(format!("d:{};", php_float_str(*f)).as_bytes());
        }
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

/// Format an `f64` exactly as PHP's `serialize()` does
/// (`zend_gcvt(val, -1, '.', 'E', ...)`, `Zend/zend_strtod.c`): the shortest
/// round-trip digit sequence (same digits Rust's `Display` produces), with
/// E-notation only when the first digit's position `decpt` (value =
/// 0.d₁d₂… × 10^decpt) satisfies `decpt > 17 || decpt < -3`, and E-forms
/// always carry at least one digit after the decimal point (`1.0E+17`,
/// `2.5E-7`, `5.0E-324`). Non-finite values use PHP's exact tokens
/// (`d:NAN;`, `d:INF;`, `d:-INF;` — what `serialize(NAN)` emits).
/// Format an `f64` exactly as PHP's `serialize()` does
/// (`zend_gcvt(val, -1, '.', 'E', ...)`, `Zend/zend_strtod.c`): the shortest
/// round-trip digit sequence, with E-notation only when the first digit's
/// position `decpt` (value = 0.d₁d₂… × 10^decpt) satisfies
/// `decpt > 17 || decpt < -3`, and E-forms always carrying at least one
/// digit after the decimal point (`1.0E+17`, `2.5E-7`, `5.0E-324`).
/// Non-finite values use PHP's exact tokens (`d:NAN;`, `d:INF;`,
/// `d:-INF;` — what `serialize(NAN)` emits).
///
/// The digit source is Rust's `{:e}` (shortest round-trip, always
/// scientific, one digit before the decimal point) — the same digits PHP's
/// `dtoa` shortest mode produces.
fn php_float_str(f: f64) -> String {
    if f.is_nan() {
        return "NAN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".to_string() } else { "-INF".to_string() };
    }
    let s = format!("{f:e}");
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.as_str()),
    };
    let (mantissa, exp) = rest.split_once(['e', 'E']).unwrap();
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // `{:e}` always has exactly one digit before the decimal point, so
    // the first digit sits at position 1 + exp (decpt convention above).
    let decpt: i32 = 1 + exp;

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-3..=17).contains(&decpt) {
        // E form (`decpt > 17 || decpt < -3` in `zend_gcvt`): d.dddE±exp
        // (exponent = decpt - 1, always signed).
        let e_exp = decpt - 1;
        out.push(digits.as_bytes()[0] as char);
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('E');
        if e_exp < 0 {
            out.push('-');
            out.push_str(&(-e_exp).to_string());
        } else {
            out.push('+');
            out.push_str(&e_exp.to_string());
        }
    } else if decpt < 0 {
        // `0.` + zeros + digits.
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        // Standard: digits, decimal point where it falls, zero-padding right
        // (here 0 <= decpt <= 17).
        let decpt = decpt as usize;
        for i in 0..decpt {
            if i < digits.len() {
                out.push(digits.as_bytes()[i] as char);
            } else {
                out.push('0');
            }
        }
        if decpt < digits.len() {
            if decpt == 0 {
                out.push('0'); // zero before the decimal point
            }
            out.push('.');
            out.push_str(&digits[decpt..]);
        }
    }
    out
}

fn write_key(out: &mut Vec<u8>, k: &Key) {
    match k {
        // PHP's `php_var_serialize` writes `i:<n>;` for every integer key
        // (list positions included) and `unserialize()` rejects the bare
        // form — so the bare form must never be emitted.
        Key::Int(i) => out.extend_from_slice(format!("i:{i};").as_bytes()),
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
            // PHP's non-finite tokens (`d:NAN;`, `d:INF;`, `d:-INF;` —
            // exactly what `serialize(NAN)`/`serialize(INF)` emit).
            let (v, np) = if s.get(p..).map(|w| w == b"NAN;").unwrap_or(false) {
                (f64::NAN, p + 4)
            } else if s.get(p..).map(|w| w == b"-INF;").unwrap_or(false) {
                (f64::NEG_INFINITY, p + 5)
            } else if s.get(p..).map(|w| w == b"INF;").unwrap_or(false) {
                (f64::INFINITY, p + 4)
            } else {
                let (v, np) = parse_float(s, p)?;
                let mut p2 = np;
                expect_char(s, &mut p2, b';')?;
                (v, p2)
            };
            p = np;
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
            // The count is attacker-storable (a corrupt `a:<huge>:{` row
            // must not reserve `huge` elements — `with_capacity(i64::MAX)`
            // panics with "capacity overflow"). PHP `unserialize()` just
            // fails on such data; the parse below errors on the first
            // missing element, so only the *reservation* is bounded.
            let mut items = Vec::with_capacity(count.min(1024));
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
    } else if s[pos] == b'i' {
        // PHP's form: `i:<int>;`.
        let mut p = pos + 1;
        expect_char(s, &mut p, b':')?;
        let (v, np) = parse_int(s, p)?;
        let mut p2 = np;
        expect_char(s, &mut p2, b';')?;
        Ok((Key::Int(v), p2))
    } else {
        // Legacy bare form (`<int>;`) — written by this port before the
        // `i:` fix; PHP never emits it, but rows already in the wild must
        // still decode.
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
        && (s[pos].is_ascii_digit()
            || s[pos] == b'.'
            || s[pos] == b'e'
            || s[pos] == b'E'
            || s[pos] == b'+'
            || s[pos] == b'-')
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

    #[test]
    fn exact_php_int_key_output() {
        // Ground truth: PHP 8.5.10 `serialize()` output (generated with the
        // reference's PHP) — `i:` prefix on *every* integer key, list
        // positions included.
        let v = Value::Array(vec![
            (Key::Bytes(b"id".to_vec()), Value::Int(7)),
            (
                Key::Bytes(b"tags".to_vec()),
                Value::Array(vec![
                    (Key::Int(0), s(b"rss")),
                    (Key::Int(1), s(b"xml")),
                ]),
            ),
            (Key::Bytes(b"enabled".to_vec()), Value::Bool(true)),
        ]);
        assert_eq!(
            serialize(&v),
            b"a:3:{s:2:\"id\";i:7;s:4:\"tags\";a:2:{i:0;s:3:\"rss\";i:1;s:3:\"xml\";}s:7:\"enabled\";b:1;}"
        );

        // Negative and non-dense keys stay `i:` too.
        let v = Value::Array(vec![(Key::Int(-1), s(b"x"))]);
        assert_eq!(serialize(&v), b"a:1:{i:-1;s:1:\"x\";}");
        let v = Value::Array(vec![
            (Key::Int(0), s(b"a")),
            (Key::Int(2), s(b"b")),
        ]);
        assert_eq!(serialize(&v), b"a:2:{i:0;s:1:\"a\";i:2;s:1:\"b\";}");
    }

    #[test]
    fn decode_php_written_int_keys() {
        // Rows PHP wrote (`i:` keys) must decode — the interop bar for
        // `ttrss_plugin_storage.content`.
        let bytes = b"a:2:{i:0;s:3:\"rss\";i:1;s:3:\"xml\";}";
        let v = deserialize(bytes).unwrap();
        assert_eq!(
            v,
            Value::Array(vec![(Key::Int(0), s(b"rss")), (Key::Int(1), s(b"xml"))])
        );
    }

    #[test]
    fn decode_legacy_bare_int_keys() {
        // Rows this port wrote before the `i:` fix still decode.
        let bytes = b"a:2:{0;s:3:\"rss\";1;s:3:\"xml\";}";
        let v = deserialize(bytes).unwrap();
        assert_eq!(
            v,
            Value::Array(vec![(Key::Int(0), s(b"rss")), (Key::Int(1), s(b"xml"))])
        );
    }

    #[test]
    fn exact_php_float_output() {
        // Ground truth: PHP 8.5.10 `serialize()` output for each value
        // (`zend_gcvt`, `serialize_precision = -1`).
        let cases: &[(f64, &[u8])] = &[
            (1.0, b"d:1;"),
            (150.0, b"d:150;"),
            (-0.0, b"d:-0;"),
            (0.1, b"d:0.1;"),
            (0.30000000000000004, b"d:0.30000000000000004;"),
            (1e15, b"d:1000000000000000;"),
            (1e16, b"d:10000000000000000;"),
            (1e17, b"d:1.0E+17;"),
            (1e18, b"d:1.0E+18;"),
            (1e-3, b"d:0.001;"),
            (1e-4, b"d:0.0001;"),
            (1e-5, b"d:1.0E-5;"),
            (2.5e-7, b"d:2.5E-7;"),
            (123456789.123456, b"d:123456789.123456;"),
            (-2.5, b"d:-2.5;"),
            (9999999999999998.0, b"d:9999999999999998;"),
            (5e-324, b"d:5.0E-324;"),
        ];
        for (v, expected) in cases {
            assert_eq!(serialize(&Value::Float(*v)), *expected, "failed for {v:?}");
        }

        // Non-finite values: PHP's exact tokens (`serialize(NAN)` etc.).
        assert_eq!(serialize(&Value::Float(f64::NAN)), b"d:NAN;");
        assert_eq!(serialize(&Value::Float(f64::INFINITY)), b"d:INF;");
        assert_eq!(serialize(&Value::Float(f64::NEG_INFINITY)), b"d:-INF;");
    }

    #[test]
    fn roundtrip_floats_and_tokens() {
        for f in [1.0, 0.1, 1e17, 2.5e-7, -2.5, 5e-324] {
            let back = deserialize(&serialize(&Value::Float(f))).unwrap();
            assert_eq!(back, Value::Float(f), "failed for {f:?}");
        }
        // NAN has no f64 `PartialEq` equality — check the token form.
        assert_eq!(serialize(&deserialize(b"d:NAN;").unwrap()), b"d:NAN;");
        assert_eq!(
            deserialize(b"d:-INF;").unwrap(),
            Value::Float(f64::NEG_INFINITY)
        );
        assert_eq!(deserialize(b"d:INF;").unwrap(), Value::Float(f64::INFINITY));
    }

    /// A huge (corrupt-row-storable) array count must be a parse *error*, not
    /// a panic: `Vec::with_capacity(i64::MAX)` used to blow the reservation
    /// with "capacity overflow" on a `a:<huge>:{` `ttrss_plugin_storage` /
    /// session row. PHP `unserialize()` just returns `false` there.
    #[test]
    fn huge_array_count_is_err_not_panic() {
        let max = i64::MAX;
        let s = format!("a:{max}:{{i:0;i:0;}}");
        assert!(deserialize(s.as_bytes()).is_err());
    }

    /// A large-but-honest count (beyond the 1024 reservation cap) still
    /// deserializes — the cap bounds only the initial reservation.
    #[test]
    fn count_above_reservation_cap_parses() {
        let n = 5000;
        let mut s = format!("a:{n}:{{");
        for i in 0..n {
            s.push_str(&format!("i:{i};i:{i};"));
        }
        s.push('}');
        let v = deserialize(s.as_bytes()).unwrap();
        let items = v.as_array().expect("array");
        assert_eq!(items.len(), n);
        assert_eq!(items[n - 1].1, Value::Int((n - 1) as i64));
    }
}
