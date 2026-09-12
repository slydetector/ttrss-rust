//! Port of the tt-rss `UrlHelper` URL-validation helpers (`UrlHelper::validate`
//! + its collaborators). PHP source: `tt-rss/classes/UrlHelper.php`.
//!
//! `validate` normalizes a URL (protocol-relative fix, IDNA host) and validates
//! it (scheme, host, port). `extended_filtering` adds the SSRF guard
//! (standard-ports-only + disallowed-IP check), which the feed-update path uses
//! to refuse requests to loopback/private/reserved addresses.

use crate::util::clean_str;

/// The components `parse_url` returns (the subset `build_url`/`validate` use).
#[derive(Clone, Debug, Default)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: Option<String>,
    path: String,
    query: String,
    fragment: String,
}

/// PHP `UrlHelper::validate($url, $extended_filtering)`. Returns the rebuilt,
/// normalized URL, or `None` if it is invalid.
pub fn validate(url: &str, extended: bool) -> Option<String> {
    let url = clean_str(url);

    // fix protocol-relative URLs
    let url = if url.starts_with("//") {
        format!("https:{}", url)
    } else {
        url
    };

    let mut tokens = parse_url(&url)?;

    // `empty($tokens['host'])`
    if tokens.host.is_empty() {
        return None;
    }

    // scheme must be http/https
    let scheme = tokens.scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }

    // convert an IDNA (non-ASCII) hostname to punycode, if possible
    if !tokens.host.is_ascii() {
        match domain_to_ascii(&tokens.host) {
            Ok(puny) => tokens.host = puny,
            Err(_) => return None,
        }
    }

    // separate set of tokens with a urlencoded path (for filter_var only — the
    // original URL is what we return / request).
    let mut tokens_fv = tokens.clone();
    if !tokens_fv.path.is_empty() {
        tokens_fv.path = tokens_fv
            .path
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
    }

    let out = build_url(&tokens);
    let out_fv = build_url(&tokens_fv);

    if !filter_validate_url(&out_fv) {
        return None;
    }

    if std::env::var("IS_INTEGRATION_TESTING").is_err() && extended {
        let port = tokens.port.as_deref().unwrap_or("");
        if !(port == "80" || port == "443" || port.is_empty()) {
            return None;
        }
        if has_disallowed_ip(&tokens, true) {
            return None;
        }
    }

    Some(out)
}

/// PHP `parse_url` (the lenient form). Returns `None` only if the URL is so
/// malformed PHP returns `false`; otherwise the components it can find.
fn parse_url(url: &str) -> Option<ParsedUrl> {
    let scheme_end = scheme_colon(url);
    let (scheme, after) = match scheme_end {
        Some(idx) => (url[..idx].to_string(), &url[idx + 1..]),
        None => (String::new(), url),
    };

    if !after.starts_with("//") {
        // scheme:path (e.g. mailto:a@b) or a bare path — no host.
        let (path, query, fragment) = split_path_query_fragment(after);
        return Some(ParsedUrl {
            scheme,
            host: String::new(),
            port: None,
            path,
            query,
            fragment,
        });
    }

    let after = &after[2..];
    let auth_end = after
        .find(|c| ['/', '?', '#'].contains(&c))
        .unwrap_or(after.len());
    let authority = &after[..auth_end];
    let rest = &after[auth_end..];

    // drop an optional `user:pass@`
    let hostport = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };

    let (host, port) = split_host_port(hostport);
    let (path, query, fragment) = split_path_query_fragment(rest);

    Some(ParsedUrl {
        scheme,
        host,
        port,
        path,
        query,
        fragment,
    })
}

/// `build_url($parts)` — reassemble `scheme://host[:port][path][?query][#frag]`.
fn build_url(t: &ParsedUrl) -> String {
    let mut s = format!("{}://{}", t.scheme, t.host);
    if let Some(p) = &t.port {
        s.push(':');
        s.push_str(p);
    }
    s.push_str(&t.path);
    if !t.query.is_empty() {
        s.push('?');
        s.push_str(&t.query);
    }
    if !t.fragment.is_empty() {
        s.push('#');
        s.push_str(&t.fragment);
    }
    s
}

/// `filter_var($url, FILTER_VALIDATE_URL)` (the parts that can still fail after
/// `validate`'s own scheme/host checks): a well-formed `scheme://authority`, a
/// valid host (domain, dotted IPv4, or bracketed IPv6), and a numeric port in
/// 0..=65535.
pub(crate) fn filter_validate_url(url: &str) -> bool {
    let idx = match scheme_colon(url) {
        Some(i) => i,
        None => return false,
    };
    if !url[idx..].starts_with("://") {
        return false;
    }
    let rest = &url[idx + 3..];
    let auth_end = rest
        .find(|c| ['/', '?', '#'].contains(&c))
        .unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    if authority.is_empty() {
        return false;
    }
    let hostport = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let (host, port) = split_host_port(hostport);
    if host.is_empty() || !valid_host(&host) {
        return false;
    }
    if let Some(p) = port {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        match p.parse::<u32>() {
            Ok(n) if n <= 65535 => {}
            _ => return false,
        }
    }
    true
}

/// A host that `filter_var` accepts: an ASCII domain / dotted-IPv4 literal
/// (alnum, `.`, `-`), or a bracketed IPv6 address. Spaces and other bytes fail.
fn valid_host(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    if host.starts_with('[') {
        if !host.ends_with(']') {
            return false;
        }
        let inner = &host[1..host.len() - 1];
        return !inner.is_empty()
            && inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':');
    }
    host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Index of the `:` that terminates the URL scheme, or `None` if there is no
/// valid leading `scheme:` (PHP `parse_url` scheme = `[a-zA-Z][a-zA-Z0-9+.-]*:`).
fn scheme_colon(url: &str) -> Option<usize> {
    let b = url.as_bytes();
    if b.is_empty() || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' {
            i += 1;
        } else if c == b':' {
            return Some(i);
        } else {
            return None;
        }
    }
    None
}

/// Split the host[:port] part (after any `user@`) into (host, port). A leading
/// `[` denotes a bracketed IPv6 host; an empty port (trailing `:`) is dropped,
/// and an all-digit port is canonicalized to its integer form (PHP `parse_url`
/// casts numeric ports to int).
fn split_host_port(hostport: &str) -> (String, Option<String>) {
    if hostport.starts_with('[') {
        match hostport.find(']') {
            Some(close) => {
                let host = hostport[..=close].to_string();
                let after = &hostport[close + 1..];
                let port = after.strip_prefix(':').and_then(normalize_port);
                return (host, port);
            }
            None => return (hostport.to_string(), None),
        }
    }
    match hostport.rfind(':') {
        Some(colon) => {
            let host = hostport[..colon].to_string();
            let port = hostport[colon + 1..].to_string();
            (host, normalize_port(&port))
        }
        None => (hostport.to_string(), None),
    }
}

/// A numeric port is reduced to its canonical integer string; a non-numeric
/// port is returned unchanged; an empty port becomes `None`.
fn normalize_port(port: &str) -> Option<String> {
    if port.is_empty() {
        return None;
    }
    if port.bytes().all(|b| b.is_ascii_digit()) {
        // strip leading zeros (keep a single `0`)
        let trimmed = port.trim_start_matches('0');
        return Some(if trimmed.is_empty() { "0".into() } else { trimmed.to_string() });
    }
    Some(port.to_string())
}

/// Split a (post-authority) string into (path, query, fragment).
fn split_path_query_fragment(s: &str) -> (String, String, String) {
    let (before_frag, fragment) = match s.find('#') {
        Some(h) => (&s[..h], s[h + 1..].to_string()),
        None => (s, String::new()),
    };
    let (path, query) = match before_frag.find('?') {
        Some(q) => (before_frag[..q].to_string(), before_frag[q + 1..].to_string()),
        None => (before_frag.to_string(), String::new()),
    };
    (path, query, fragment)
}

/// PHP `idn_to_ascii($host, IDNA_NONTRANSITIONAL_TO_ASCII, INTL_IDNA_VARIANT_UTS46)`.
fn domain_to_ascii(host: &str) -> Result<String, idna::Errors> {
    idna::domain_to_ascii(host)
}

/// PHP `UrlHelper::has_disallowed_ip($tokens, $validate_resolved_ip)` — true if
/// the URL targets localhost / a loopback / link-local / reserved address.
/// (Reached via `validate` only with a standard port, so the non-standard-port
/// private/ULA branches never fire there, but they are ported for reuse.)
fn has_disallowed_ip(tokens: &ParsedUrl, validate_resolved_ip: bool) -> bool {
    if tokens.host.is_empty() {
        return false;
    }
    let host_lower = tokens.host.to_ascii_lowercase();
    let host = host_lower.trim_matches(|c| c == '[' || c == ']' || c == '.');
    let port = tokens.port.as_deref();
    let is_standard_port = port.is_none() || port == Some("80") || port == Some("443");

    if host.is_empty() || host == "localhost" {
        return true;
    }

    let mut ips: Vec<String> = Vec::new();
    if is_ipv4(host) || is_ipv6(host) {
        ips.push(host.to_string());
    } else if let Some(canon) = canonicalize_ipv4_literal(host) {
        ips.push(canon);
    } else if validate_resolved_ip {
        let addr = (host, 0u16);
        if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&addr) {
            for a in addrs {
                ips.push(a.ip().to_string());
            }
        }
    }

    if ips.is_empty() {
        return validate_resolved_ip;
    }

    for ip in &ips {
        let bin = ip_to_bytes(ip);
        // reject loopback, link-local, and reserved ranges (regardless of port)
        if !valid_ip_no_res_range(&bin) {
            return true;
        }
        // IPv6 ULA (fc00::/7) on a non-standard port
        if !is_standard_port
            && bin
                .as_ref()
                .is_some_and(|b| b.len() == 16 && b[0] & 0xfe == 0xfc)
        {
            return true;
        }
        // reject RFC1918 / private ranges on a non-standard port
        if !is_standard_port && !valid_ip_no_priv_range(&bin) {
            return true;
        }
    }

    false
}

/// PHP `UrlHelper::canonicalize_ipv4_literal($host)` — a dotted IPv4 literal
/// using octal/hex/decimal octets (e.g. `0177.0.0.1`, `0x7f.0.0.1`) → a
/// canonical dotted-quad, or `None` if it is not such a literal.
fn canonicalize_ipv4_literal(host: &str) -> Option<String> {
    let parts: Vec<&str> = host.split('.').collect();
    // 1 to 4 non-empty base-0 (decimal / 0x-hex / 0-octal) octets
    if parts.len() > 4 || parts.iter().any(|p| p.is_empty() || !is_int_like(p)) {
        return None;
    }
    let n = parts.len();
    let mut val: u32 = 0;
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == n - 1;
        let octet = parse_int_any_base(part)?;
        let max = if is_last {
            (1u32 << (8 * (5 - n))) - 1
        } else {
            255
        };
        if octet > max {
            return None;
        }
        if is_last {
            val = (val << (8 * (5 - n))) | octet;
        } else {
            val = (val << 8) | octet;
        }
    }
    Some(format!("{}.{}.{}.{}", (val >> 24) & 0xff, (val >> 16) & 0xff, (val >> 8) & 0xff, val & 0xff))
}

/// A non-empty string of base-0 integer characters (decimal / 0x-hex / 0-octal).
fn is_int_like(p: &str) -> bool {
    !p.is_empty() && p.bytes().all(|b| b.is_ascii_hexdigit() || b == b'x')
}
/// `intval($part, 0)`: base-0 (`0x` hex, `0` octal, otherwise decimal).
fn parse_int_any_base(part: &str) -> Option<u32> {
    if part.is_empty() {
        return Some(0);
    }
    let lower = part.to_ascii_lowercase();
    let (digits, radix) = if let Some(s) = lower.strip_prefix("0x") {
        (s, 16)
    } else if lower.starts_with('0') && lower.len() > 1 {
        (lower.as_str(), 8)
    } else {
        (lower.as_str(), 10)
    };
    u32::from_str_radix(digits, radix).ok()
}

/// `inet_pton($ip)` — the IP's binary form (4 bytes IPv4 / 16 bytes IPv6).
fn ip_to_bytes(ip: &str) -> Option<Vec<u8>> {
    let inner = ip.trim_start_matches('[').trim_end_matches(']');
    let is_v6 = inner.contains(':');
    if is_v6 {
        inner.parse::<std::net::Ipv6Addr>().ok().map(|a| a.octets().to_vec())
    } else {
        inner.parse::<std::net::Ipv4Addr>().ok().map(|a| a.octets().to_vec())
    }
}

/// `filter_var($ip, FILTER_VALIDATE_IP, FILTER_FLAG_NO_RES_RANGE)` — false for
/// loopback / link-local / reserved / unspecified ranges (any port).
fn valid_ip_no_res_range(bin: &Option<Vec<u8>>) -> bool {
    match bin.as_ref() {
        None => true,
        Some(b) if b.len() == 4 => {
            let a = b[0];
            !(a == 0 || a == 127 || (a == 169 && b[1] == 254) || a >= 240)
        }
        Some(b) if b.len() == 16 => {
            // 0000::/8 (unspecified/loopback) and fe80::/10 (link-local)
            let zero8 = b[0] == 0;
            let fe80 = b[0] == 0xfe && b[1] & 0xc0 == 0x80;
            !(zero8 || fe80)
        }
        _ => true,
    }
}

/// `filter_var($ip, FILTER_VALIDATE_IP, FILTER_FLAG_NO_PRIV_RANGE)` — false for
/// RFC1918 private (IPv4) and ULA fc00::/7 (IPv6).
fn valid_ip_no_priv_range(bin: &Option<Vec<u8>>) -> bool {
    match bin.as_ref() {
        None => true,
        Some(b) if b.len() == 4 => {
            !(b[0] == 10
                || (b[0] == 172 && (16..=31).contains(&b[1]))
                || (b[0] == 192 && b[1] == 168))
        }
        Some(b) if b.len() == 16 => {
            let g0 = u16::from_be_bytes([b[0], b[1]]);
            !(0xfc00..=0xfdff).contains(&g0)
        }
        _ => true,
    }
}

/// `filter_var($host, FILTER_VALIDATE_IP)` — a well-formed IPv4 or IPv6 literal.
fn is_ipv4(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
}
fn is_ipv6(host: &str) -> bool {
    host.parse::<std::net::Ipv6Addr>().is_ok()
}
