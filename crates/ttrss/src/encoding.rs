//! Feed-byte decoding at the fetch boundary.
//!
//! The reference hands the fetched *bytes* to libxml2, which resolves the
//! charset itself: BOM, then the XML declaration's `encoding=`, defaulting to
//! UTF-8. The HTTP Content-Type charset is never seen (the PHP fetch hands raw
//! bytes to `simplexml_load_string`), so parity does not consult it either.
//!
//! This module reproduces that resolution with [`encoding_rs`] (the WHATWG
//! set — legacy aliases included, e.g. declared `gb2312` decodes as GB18030,
//! the superset the wild feeds actually use), plus one deliberate deviation:
//!
//! **Lying declarations.** A feed that declares a non-UTF-8 charset but whose
//! bytes are valid UTF-8 with non-ASCII content is decoded as UTF-8, not with
//! the declared charset. Real-world example: `special.cyzone.cn/rss/`
//! declares `encoding="gbk"` over UTF-8 bytes; readers that trust the
//! declaration render it as mojibake (MrRSS issue #876). The reference shares
//! that flaw (libxml trusts the declaration); deviating here is a
//! known-good departure.
//!
//! Invalid bytes in a declared charset decode lossily (U+FFFD) and parsing
//! continues — the same posture the port already takes for invalid UTF-8
//! (the "inverse gap" vs the reference, which rejects the whole feed).

/// Decode a fetched feed body into the UTF-8 string the parser expects.
pub fn decode_feed_bytes(body: &[u8]) -> String {
    // 1. BOM wins (XML + encoding_rs both agree on this priority).
    let bom = encoding_rs::Encoding::for_bom(body);

    // 2. Read the declared encoding from the raw bytes (it must be read
    // before any conversion, or the declaration is already mangled).
    let bom_len = bom.as_ref().map(|(_, n)| *n).unwrap_or(0);
    let declared = declared_encoding(&body[bom_len..]);

    match (bom, declared.as_deref()) {
        // UTF-8 BOM: UTF-8, full stop.
        (Some((enc, _)), _) if *enc == *encoding_rs::UTF_8 => {
            from_utf8_lossy_string(&body[bom_len..])
        }

        // Some other BOM (the WHATWG set only defines UTF-8/UTF-16 BOMs):
        // decode with the BOM's encoding.
        (Some((enc, _)), _) => enc.decode(&body[bom_len..]).0.into_owned(),

        // Declared UTF-8: UTF-8, full stop.
        (None, Some(enc)) if is_utf8_name(enc) => from_utf8_lossy_string(body),

        // Declared UTF-16 (no BOM): WHATWG defaults bare "utf-16" to LE.
        (None, Some(enc)) if is_utf16_name(enc) => {
            let enc = if enc.eq_ignore_ascii_case("utf-16be") {
                encoding_rs::UTF_16BE
            } else {
                encoding_rs::UTF_16LE
            };
            enc.decode(body).0.into_owned()
        }

        // 3. No BOM, no declaration: default UTF-8 (the reference default).
        (None, None) => from_utf8_lossy_string(body),

        // 4. Declared non-UTF-8: trust the bytes over a lying declaration —
        // valid UTF-8 with non-ASCII content is UTF-8 (the cyzone case).
        (None, Some(enc)) => {
            if let Ok(s) = std::str::from_utf8(body)
                && !s.is_ascii()
            {
                return s.to_string();
            }
            match encoding_rs::Encoding::for_label(enc.as_bytes()) {
                Some(d) => d.decode(body).0.into_owned(),
                // Unresolvable name: the reference (libxml) would reject the
                // feed; we stay lenient and fall back to the port's
                // established posture (lossy UTF-8, continue).
                None => from_utf8_lossy_string(body),
            }
        }
    }
}

fn from_utf8_lossy_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// True for the UTF-8 names a declaration may use.
fn is_utf8_name(s: &str) -> bool {
    s.eq_ignore_ascii_case("utf-8") || s.eq_ignore_ascii_case("utf8")
}

/// True for the UTF-16 family names a declaration may use.
fn is_utf16_name(s: &str) -> bool {
    s.eq_ignore_ascii_case("utf-16")
        || s.eq_ignore_ascii_case("utf-16le")
        || s.eq_ignore_ascii_case("utf-16be")
        || s.eq_ignore_ascii_case("unicode")
        || s.eq_ignore_ascii_case("ucs-2")
}

/// Extract `encoding="…"` from a leading XML declaration on the raw bytes.
///
/// The declaration must begin at the start of the stream (XML §7.1), so this
/// scans only a small prefix and only a leading `<?xml` tag. Lenient on the
/// surrounding whitespace inside the tag; the value is unquoted as XML allows
/// (single or double quotes). Returns `None` when there is no declaration or
/// no readable encoding attribute.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let prefix = &bytes[..bytes.len().min(200)];
    if !prefix.starts_with(b"<?xml") {
        return None;
    }
    let end = prefix.windows(2).position(|w: &[u8]| w == b"?>");
    let end = end?;

    let mut i = 4; // skip "<?xml"
    while i < end {
        let mut name_end = i;
        while name_end < end {
            let c = prefix[name_end];
            if c == b'"' || c == b'\'' || c == b'=' {
                break;
            }
            name_end += 1;
        }
        let name = String::from_utf8_lossy(&prefix[i..name_end])
            .trim()
            .to_string();

        if name_end < end && prefix[name_end] == b'=' {
            name_end += 1;
            while name_end < end && prefix[name_end].is_ascii_whitespace() {
                name_end += 1;
            }
        }
        let Some(quote) = prefix.get(name_end) else {
            break;
        };
        if *quote != b'"' && *quote != b'\'' {
            break;
        }
        let mut value_end = name_end + 1;
        while value_end < end && prefix[value_end] != *quote {
            value_end += 1;
        }
        if value_end >= end {
            break;
        }
        let value = String::from_utf8_lossy(&prefix[name_end + 1..value_end]).to_string();
        if name.eq_ignore_ascii_case("encoding") && !value.is_empty() {
            return Some(value);
        }
        i = value_end + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenate raw fragments (test helper).
    fn raw(parts: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p);
        }
        v
    }

    /// Encode `s` with the given WHATWG label (test helper).
    fn enc(label: &[u8], s: &str) -> Vec<u8> {
        encoding_rs::Encoding::for_label(label)
            .unwrap()
            .encode(s)
            .0
            .into_owned()
    }

    fn feed(decl: Option<&str>, body: &str) -> Vec<u8> {
        let xml = format!(
            "<{}rss version=\"2.0\"><channel><title>{}</title></channel></rss>",
            decl.map(|d| format!(r#"<?xml version="1.0" encoding="{d}"?>"#))
                .unwrap_or_default(),
            body,
        );
        xml.into_bytes()
    }

    #[test]
    fn plain_utf8_no_declaration() {
        let r = feed(None, "héllo");
        assert_eq!(decode_feed_bytes(&r), String::from_utf8(r).unwrap());
    }

    #[test]
    fn utf8_declaration() {
        let r = feed(Some("UTF-8"), "hello");
        assert_eq!(decode_feed_bytes(&r), String::from_utf8(r).unwrap());
    }

    #[test]
    fn utf8_bom() {
        let r = raw(&[
            b"\xEF\xBB\xBF",
            b"<rss version=\"2.0\"><title>h\xC3\xA9llo</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("héllo"));
        assert!(!s.contains('\u{feff}'));
    }

    #[test]
    fn utf16le_bom() {
        let text = "<title>héllo 世界</title>";
        let mut r = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            r.extend_from_slice(&u.to_le_bytes());
        }
        let s = decode_feed_bytes(&r);
        assert!(s.contains("héllo 世界"));
    }

    #[test]
    fn utf16be_bom() {
        let text = "<title>héllo</title>";
        let mut r = vec![0xFE, 0xFF];
        for u in text.encode_utf16() {
            r.extend_from_slice(&u.to_be_bytes());
        }
        let s = decode_feed_bytes(&r);
        assert!(s.contains("héllo"));
    }

    #[test]
    fn iso88592_declared() {
        // twojepc.pl shape (FreshRSS #6133): declares iso-8859-2, bytes are Latin-2.
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"iso-8859-2\"?><rss><title>Tablety przemys",
            &[0xB3], // l-with-stroke (its real iso-8859-2 position) in iso-8859-2
            b"owe</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("przemysłowe"), "got: {s}");
    }

    #[test]
    fn gbk_declared_gbk_bytes() {
        // The Reddit case: old Chinese CMS, declares gbk, bytes are GBK.
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"gbk\"?><rss><title>",
            &enc(b"gbk", "新漫画发布"),
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("新漫画发布"), "got: {s}");
    }

    #[test]
    fn gb2312_declared_gbk_bytes_superset() {
        // The Baidu case: declares gb2312, bytes use GBK/GB18030 characters.
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"gb2312\"?><rss><title>",
            &enc(b"gb2312", "百度国内焦点新闻"),
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("百度国内焦点新闻"), "got: {s}");
    }

    #[test]
    fn gbk_declared_utf8_bytes_lies() {
        // The cyzone case (MrRSS #876): declares gbk, bytes are UTF-8.
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"gbk\"?><rss><title>",
            "上海电气成立变压器公司".as_bytes(),
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("上海电气成立变压器公司"), "got: {s}");
    }

    #[test]
    fn gbk_declared_pure_ascii() {
        // ASCII-only: both charsets decode identically.
        let r = feed(Some("gbk"), "plain");
        assert_eq!(decode_feed_bytes(&r), String::from_utf8(r).unwrap());
    }

    #[test]
    fn windows1252_declared() {
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"windows-1252\"?><rss><title>Gr",
            &[0xF6, 0xDF], // o-umlaut + sharp-s
            b"e</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("Größe"), "got: {s}");
    }

    #[test]
    fn shift_jis_declared() {
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><rss><title>",
            &enc(b"shift_jis", "新しい漫画"),
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("新しい漫画"), "got: {s}");
    }

    #[test]
    fn euc_jp_declared() {
        let r = raw(&[
            b"<?xml version=\"1.0\" encoding=\"euc-jp\"?><rss><title>",
            &enc(b"euc-jp", "記事"),
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("記事"), "got: {s}");
    }

    #[test]
    fn unknown_declared_charset_falls_back_to_utf8() {
        let r = feed(Some("rot13"), "hello");
        assert_eq!(decode_feed_bytes(&r), String::from_utf8(r).unwrap());
    }

    #[test]
    fn declaration_not_at_start_is_ignored() {
        // Leading whitespace before <?xml: not a declaration per the XML spec.
        let r = b"  <?xml version=\"1.0\" encoding=\"gbk\"?><rss><t>a</t></rss>";
        let s = decode_feed_bytes(r);
        assert!(s.contains("a"));
    }

    #[test]
    fn single_quoted_declaration() {
        let r = raw(&[
            b"<?xml version='1.0' encoding='iso-8859-2'?><rss><title>a",
            &[0xB3], // l-with-stroke (its real iso-8859-2 position)
            b"</title></rss>",
        ]);
        let s = decode_feed_bytes(&r);
        assert!(s.contains("ał"), "got: {s}");
    }
}
