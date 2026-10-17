//! Port of the tt-rss `UrlHelper` URL-validation helpers (`UrlHelper::validate`
//! + its collaborators). PHP source: `tt-rss/classes/UrlHelper.php`.
//!
//! `validate` normalizes a URL (protocol-relative fix, IDNA host) and validates
//! it (scheme, host, port). `extended_filtering` adds the SSRF guard
//! (standard-ports-only + disallowed-IP check), which the feed-update path uses
//! to refuse requests to loopback/private/reserved addresses.

use crate::core::config::VERSION;
use crate::util::clean_str;
use crate::with_trailing_slash;
use std::net::ToSocketAddrs;
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
        return !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':');
    }
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
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
        return Some(if trimmed.is_empty() {
            "0".into()
        } else {
            trimmed.to_string()
        });
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
        Some(q) => (
            before_frag[..q].to_string(),
            before_frag[q + 1..].to_string(),
        ),
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
        let mut bin = ip_to_bytes(ip);
        // Unwrap IPv4-mapped IPv6 (`::ffff:127.0.0.1` → `127.0.0.1`) before
        // the range checks — the PHP canonicalizes + unwraps via
        // `inet_pton`/`inet_ntop` for the same reason: without this, a
        // mapped *public* IP trips the 0000::/8 "zero bytes" reserved rule.
        if let Some(b) = &bin
            && b.len() == 16
            && let Ok(arr) = <[u8; 16]>::try_from(b.as_slice())
            && let Some(v4) = std::net::Ipv6Addr::from(arr).to_ipv4_mapped()
        {
            bin = Some(v4.octets().to_vec());
        }
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
    Some(format!(
        "{}.{}.{}.{}",
        (val >> 24) & 0xff,
        (val >> 16) & 0xff,
        (val >> 8) & 0xff,
        val & 0xff
    ))
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
        inner
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .map(|a| a.octets().to_vec())
    } else {
        inner
            .parse::<std::net::Ipv4Addr>()
            .ok()
            .map(|a| a.octets().to_vec())
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

/// PHP `UrlHelper::rewrite_relative($base_url, $rel_url)` — resolve a relative
/// URL against a base (RFC 3986 §5.2.2). An absolute `scheme://host` URL passes
/// through [`validate`]; a relative path is merged with the base's scheme, host,
/// and directory. Returns the validated URL, or `None` if it cannot be resolved
/// (mirrors the PHP `false`).
pub(crate) fn rewrite_relative(base_url: &str, rel_url: &str) -> Option<String> {
    // PHP: `if (!$rel_url) return $base_url;` — an empty relative URL is the
    // base itself (not the base's origin).
    if rel_url.is_empty() {
        return Some(base_url.to_string());
    }
    let mut rel_parts = parse_url(rel_url)?;

    // Absolute URL (has both a scheme and a host) → validate and return as-is.
    if !rel_parts.host.is_empty() && !rel_parts.scheme.is_empty() {
        return validate(rel_url, false);
    }

    let base_parts = parse_url(base_url)?;

    // Inherit the base's scheme / host / port (a relative URL has none).
    rel_parts.scheme = base_parts.scheme;
    rel_parts.host = base_parts.host;
    rel_parts.port = base_parts.port;

    // Resolve the relative path against the base directory (§5.2.2): if the base
    // path does not end in `/`, use the `dirname` of it.
    if !rel_parts.path.is_empty() {
        let mut base_path = base_parts.path.clone();
        if !base_path.ends_with('/') {
            base_path = with_trailing_slash(&php_dirname(&base_path));
        }
        if rel_parts.path.starts_with("./") {
            rel_parts.path = format!("{}{}", base_path, &rel_parts.path[2..]);
        } else if !rel_parts.path.starts_with('/') {
            rel_parts.path = format!("{}{}", base_path, rel_parts.path);
        }
        // An absolute-relative path (leading `/`) is already document-rooted.
    }

    validate(&build_url(&rel_parts), false)
}

/// PHP `dirname($path)` — the directory component (the path up to, excluding, the
/// final `/`), matching the edge cases the URL rewriter relies on:
/// `dirname("/base") == ""`, `dirname("base") == "."`, `dirname("/") == "/"`,
/// `dirname("") == "."`.
fn php_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string(); // the path was all slashes (e.g. "/")
    }
    match trimmed.rfind('/') {
        Some(0) => String::new(),                // "/base" → ""
        Some(idx) => trimmed[..idx].to_string(), // "/base/dir" → "/base"
        None => ".".to_string(),                 // "base" → "."
    }
}

// =========================================================================
// UrlHelper::fetch — the HTTP fetch layer (M6c.1)
// =========================================================================
//
// Rust port of `classes/UrlHelper.php::fetch()` + `get_user_agent()`. This is
// the shared HTTP primitive the feed updater (M6c.2), the favicon fetcher, and
// the OPML import all call. It mirrors the PHP's option set and its
// `fetch_last_*` state (returned as [`FetchResult`]): the extended-URL
// validation gate (via [`validate`]), per-hop redirect validation, the 403
// auth retry, the conditional `If-Modified-Since` request, the gzip response
// (reqwest's `gzip` feature), the `Content-Length`/size-limit guard, and the
// `effective_url` / `effective_ip` resolution.
//
// Faithful-by-default notes:
//   * The PHP uses Guzzle (PSR-7); the port uses `reqwest`. Both follow
//     redirects, honor conditional requests, and decompress gzip.
//   * Auth: `auth_type` ("basic" (default) | "digest" | "any") drives the
//     attempt (see [`AuthScheme`]). reqwest has no native digest, so a digest
//     401 challenge gets one computed `Authorization: Digest` resend for the
//     digest-capable schemes (RFC 2617 `qop=auth` + legacy no-qop; an
//     `auth-int`-only challenge is skipped). The 403 retry (the PHP retries
//     once with `CURLAUTH_ANY`) re-sends with "any" = preemptive basic + the
//     digest challenge handling — a strict superset of the first attempt.
//   * Per-request timeouts default to the PHP's FILE_FETCH_TIMEOUT (45 s) and
//     FILE_FETCH_CONNECT_TIMEOUT (15 s); M6c.7 wires the `Config` surface.

/// `UrlHelper::get_user_agent()` — the User-Agent sent on fetches:
/// `Tiny Tiny RSS/{version} (https://github.com/tt-rss/tt-rss)`. The
/// `TTRSS_HTTP_USER_AGENT` env var overrides the default (the PHP
/// `Config::HTTP_USER_AGENT` key is settable only in test envs with no UI;
/// this env is the port's settable surface for it — gap-scan §F).
fn default_user_agent() -> String {
    ua_for(std::env::var("TTRSS_HTTP_USER_AGENT").ok().as_deref())
}

fn ua_for(override_: Option<&str>) -> String {
    match override_ {
        Some(ua) if !ua.trim().is_empty() => ua.to_string(),
        _ => format!("Tiny Tiny RSS/{VERSION} (https://github.com/tt-rss/tt-rss)"),
    }
}

/// `UrlHelper::fetch()`'s options (the PHP's `$options` array). Unset fields
/// fall back to the PHP defaults.
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    pub url: String,
    pub login: Option<String>,
    pub pass: Option<String>,
    /// `"basic"` (default) | `"digest"` | `"any"`.
    pub auth_type: Option<String>,
    /// Total request timeout (default 45 s — FILE_FETCH_TIMEOUT).
    pub timeout: Option<std::time::Duration>,
    /// Connect timeout (default 15 s — FILE_FETCH_CONNECT_TIMEOUT).
    pub connect_timeout: Option<std::time::Duration>,
    /// `If-Modified-Since` value (a conditional request; skipped for POST).
    pub last_modified: Option<String>,
    /// `If-None-Match` value (an ETag; a conditional request, skipped for POST).
    pub etag: Option<String>,
    /// `User-Agent` override (default the tt-rss agent).
    pub useragent: Option<String>,
    /// Follow redirects (default `true`, up to 20 hops, each extended-validated).
    pub followlocation: Option<bool>,
    /// Max response size in bytes (default 16 MiB — MAX_DOWNLOAD_FILE_SIZE).
    pub max_size: Option<u64>,
    /// `Accept` header.
    pub http_accept: Option<String>,
    /// `Referer` header.
    pub http_referrer: Option<String>,
    /// `Accept-Encoding` header override.
    pub encoding: Option<String>,
    /// POST form params (makes this a `POST` instead of a `GET`).
    pub post_query: Option<Vec<(String, String)>>,
    /// HTTP proxy URL (e.g. `http://host:port`).
    pub proxy: Option<String>,
    /// Whether to apply the extended URL validation (the SSRF guard). Default
    /// `true` (the `Extended` filtering); `None` (the derived default) means
    /// `true`.
    pub filtering: Option<bool>,
}

/// The result of [`fetch`] — mirrors the PHP's `fetch_last_*` globals plus the
/// returned body (`None` when the fetch failed or had no content).
#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    /// The response body, or `None` if the fetch failed / had no content.
    pub body: Option<Vec<u8>>,
    /// Status line / error message. On a received response this is
    /// `"HTTP/1.1 <status> <reason>"` (even for 2xx); on a transport error it
    /// is the error text; on the empty-body case the no-content message.
    pub error: String,
    /// `-1` until a response is received; then the HTTP status code (or `-1`
    /// for a rejected redirect).
    pub error_code: i64,
    /// Final URL after redirects.
    pub effective_url: String,
    /// Resolved IP of `effective_url`'s host (the host itself on failure).
    pub effective_ip: String,
    /// `Last-Modified` response header ("" if absent).
    pub last_modified: String,
    /// `Content-Type` response header ("" if absent).
    pub content_type: String,
}

/// `UrlHelper::fetch($url, $options)` — fetch `$url` over HTTP(S) and return
/// [`FetchResult`]. See the module note above for the faithful-by-default
/// notes.
pub async fn fetch(opts: &FetchOptions) -> FetchResult {
    let mut res = FetchResult {
        error_code: -1,
        ..Default::default()
    };

    let extended = opts.filtering.unwrap_or(true);

    // Normalize the URL: ltrim whitespace, encode spaces (the PHP's
    // `ltrim($url, ' \n')` + `str_replace(' ', '%20', $url)`).
    let url = opts.url.trim_start().replace(' ', "%20");

    // Validate (extended) before fetching.
    let url = match validate(&url, extended) {
        Some(u) => u,
        None => {
            res.error = "Requested URL failed extended validation.".into();
            return res;
        }
    };

    let scheme = AuthScheme::resolve(opts.auth_type.as_deref());
    let followlocation = opts.followlocation.unwrap_or(true);
    let timeout = opts
        .timeout
        .unwrap_or_else(|| std::time::Duration::from_secs(45));
    let connect_timeout = opts
        .connect_timeout
        .unwrap_or_else(|| std::time::Duration::from_secs(15));
    let max_size = opts.max_size.unwrap_or(16 * 1024 * 1024);
    let useragent = opts.useragent.clone().unwrap_or_else(default_user_agent);

    // Redirect-validation state (shared with the redirect-policy closure).
    let invalid_redirect = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));

    let redirect_policy = if followlocation {
        reqwest::redirect::Policy::custom({
            let invalid = invalid_redirect.clone();
            move |attempt| {
                let target: String = attempt.url().to_string();
                // Cap the redirect chain (the PHP's `max: 20`).
                if attempt.previous().len() >= 20 {
                    return attempt.stop();
                }
                if validate(&target, extended).is_some() {
                    attempt.follow()
                } else {
                    *invalid.lock().unwrap() = Some(target);
                    attempt.stop()
                }
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };

    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .redirect(redirect_policy);
    if let Some(proxy) = &opts.proxy
        && let Ok(p) = reqwest::Proxy::all(proxy)
    {
        builder = builder.proxy(p);
    }
    let Ok(client) = builder.build() else {
        res.error = "Failed to build HTTP client.".into();
        return res;
    };

    // Send (with the 403 auth retry — the PHP retries once with CURLAUTH_ANY).
    let resp = match send_with_auth(&client, &url, opts, scheme, &useragent).await {
        Ok(r) => {
            let needs_retry = r.status() == reqwest::StatusCode::FORBIDDEN
                && opts
                    .login
                    .as_deref()
                    .map(|l| !l.is_empty())
                    .unwrap_or(false)
                && opts.pass.as_deref().map(|p| !p.is_empty()).unwrap_or(false)
                && scheme != AuthScheme::Any;
            if needs_retry {
                // The PHP sets `auth_type = 'any'` and re-fetches: preemptive
                // basic plus the digest challenge handling — strictly more
                // capable than the first attempt.
                match send_with_auth(&client, &url, opts, AuthScheme::Any, &useragent).await {
                    Ok(r2) => r2,
                    Err(e) => {
                        res.error = e.to_string();
                        return res;
                    }
                }
            } else {
                r
            }
        }
        Err(e) => {
            // Transport error (connection failure, DNS, timeout, proxy). The
            // PHP's GuzzleException path leaves error_code at -1.
            res.error = e.to_string();
            return res;
        }
    };

    // A redirect was rejected by the policy: the final URL is the invalid
    // target, and the PHP's on_redirect exception path leaves error_code -1.
    if let Some(invalid_target) = invalid_redirect.lock().unwrap().take() {
        res.error = "URL received during redirection failed extended validation.".into();
        res.effective_url = invalid_target;
        return res;
    }

    let status = resp.status();
    let status_code = status.as_u16() as i64;
    res.error_code = status_code;
    let reason = status.canonical_reason().unwrap_or_default();
    res.error = format!("HTTP/1.1 {status_code} {reason}");
    res.last_modified = resp
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    res.content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    res.effective_url = resp.url().to_string();

    // A 4xx/5xx response: no body is returned (the PHP returns false here).
    if status_code >= 400 {
        return res;
    }

    // Content-Length guard (before reading the body).
    if resp.content_length().is_some_and(|cl| cl > max_size) {
        res.error = "Server sent 'Content-Length' exceeding download limit".into();
        return res;
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            res.error = e.to_string();
            return res;
        }
    };
    if bytes.len() as u64 > max_size {
        res.error = "Download exceeded size limit".into();
        return res;
    }
    if bytes.is_empty() {
        res.error = "Successful response, but no content was received.".into();
        return res;
    }

    // Validate the final effective URL (extended) — the PHP's "after
    // redirection" safety check.
    if validate(&res.effective_url, extended).is_none() {
        res.error = "URL received after redirection failed extended validation.".into();
        return res;
    }

    res.effective_ip = resolve_host_ip(&res.effective_url).await;
    res.body = Some(bytes.to_vec());
    res
}

/// The `auth_type` option ("basic" (default) | "digest" | "any") resolved to
/// the attempt's auth behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthScheme {
    /// Absent or `"basic"` — the preemptive `Authorization: Basic` header.
    Basic,
    /// `"digest"` — no preemptive header; answers a digest 401 challenge.
    Digest,
    /// `"any"` (PHP `CURLAUTH_ANY`) — preemptive basic + digest challenge.
    Any,
    /// An unrecognized value — the PHP's `in_array`/`===` checks match
    /// nothing, so the request goes out unauthenticated.
    Off,
}

impl AuthScheme {
    fn resolve(opt: Option<&str>) -> Self {
        match opt {
            None | Some("basic") => Self::Basic,
            Some("digest") => Self::Digest,
            Some("any") => Self::Any,
            Some(_) => Self::Off,
        }
    }

    /// Send the preemptive `Authorization: Basic` header (basic and any).
    fn preemptive_basic(self) -> bool {
        matches!(self, Self::Basic | Self::Any)
    }

    /// Take part in the one-shot digest challenge round-trip.
    fn digest_capable(self) -> bool {
        matches!(self, Self::Digest | Self::Any)
    }
}

/// Send one attempt of [`fetch`] with the given [`AuthScheme`]. For the
/// digest-capable schemes, a `401` answered with `WWW-Authenticate: Digest`
/// triggers exactly one computed `Authorization: Digest` resend (RFC 2617
/// `qop=auth` and the legacy no-qop form).
async fn send_with_auth(
    client: &reqwest::Client,
    url: &str,
    opts: &FetchOptions,
    scheme: AuthScheme,
    useragent: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let resp = build_and_send(client, url, opts, scheme, useragent, None).await?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        && scheme.digest_capable()
        && let (Some(login), Some(pass)) = (&opts.login, &opts.pass)
        && !login.is_empty()
        && !pass.is_empty()
        && let Some(header) = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_digest_challenge)
            .and_then(|ch| digest_authorization(url, opts.post_query.is_some(), login, pass, &ch))
    {
        return build_and_send(client, url, opts, scheme, useragent, Some(&header)).await;
    }

    Ok(resp)
}

/// Build and send a single request for [`fetch`].
async fn build_and_send(
    client: &reqwest::Client,
    url: &str,
    opts: &FetchOptions,
    scheme: AuthScheme,
    useragent: &str,
    digest_header: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let is_post = opts.post_query.is_some();

    let mut req = if is_post {
        client.post(url)
    } else {
        client.get(url)
    };
    req = req.header("User-Agent", useragent);

    // Conditional request (skipped for POST, matching the PHP).
    if let Some(lm) = &opts.last_modified
        && !is_post
    {
        req = req.header("If-Modified-Since", lm);
    }
    if let Some(etag) = &opts.etag
        && !is_post
    {
        req = req.header("If-None-Match", etag);
    }
    if let Some(acc) = &opts.http_accept {
        req = req.header("Accept", acc);
    }
    if let Some(enc) = &opts.encoding {
        req = req.header("Accept-Encoding", enc);
    }
    if let Some(ref_) = &opts.http_referrer {
        req = req.header("Referer", ref_);
    }

    if let Some(digest) = digest_header {
        req = req.header(reqwest::header::AUTHORIZATION, digest);
    } else if scheme.preemptive_basic()
        && let (Some(login), Some(pass)) = (&opts.login, &opts.pass)
        && !login.is_empty()
        && !pass.is_empty()
    {
        req = req.basic_auth(login, Some(pass));
    }

    // reqwest 0.13 dropped `RequestBuilder::form`; the same
    // x-www-form-urlencoded body, built by hand.
    if let Some(post) = &opts.post_query {
        let body = post
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        req = req
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body);
    }

    req.send().await
}

/// The parameters of a `WWW-Authenticate: Digest …` challenge.
struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    /// The `qop` value list (e.g. `"auth"` / `"auth, auth-int"`); `None` =
    /// the legacy (RFC 2069) form without `qop`.
    qop: Option<String>,
}

/// Parse a `WWW-Authenticate: Digest realm="…", nonce="…", …` header (quoted
/// and bare values alike). `None` for a non-Digest scheme or a challenge
/// without `realm`/`nonce`.
fn parse_digest_challenge(header: &str) -> Option<DigestChallenge> {
    let header = header.trim();
    let rest = header
        .strip_prefix("Digest")
        .or_else(|| header.strip_prefix("digest"))?
        .trim();
    let re = regex::Regex::new(r#"([a-zA-Z]+)=(?:"([^"]*)"|(\S+))"#).ok()?;
    let mut realm = None;
    let mut nonce = None;
    let mut opaque = None;
    let mut qop = None;
    for caps in re.captures_iter(rest) {
        let value = caps.get(2).or(caps.get(3))?.as_str().to_string();
        match caps.get(1).unwrap().as_str() {
            "realm" => realm = Some(value),
            "nonce" => nonce = Some(value),
            "opaque" => opaque = Some(value),
            "qop" => qop = Some(value),
            _ => {}
        }
    }
    Some(DigestChallenge {
        realm: realm?,
        nonce: nonce?,
        opaque,
        qop,
    })
}

/// The request-target (path + query, rooted at `/`) of an absolute URL — the
/// digest `uri=` parameter and the server's `H(A2)` both use it.
fn request_uri(url: &str) -> String {
    let after = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let idx = after.find(['/', '?', '#']).unwrap_or(after.len());
    let uri = &after[idx..];
    if uri.is_empty() {
        "/".to_string()
    } else {
        uri.to_string()
    }
}

/// Build the `Authorization: Digest …` header answering a challenge (RFC 2617
/// `qop=auth`, or the legacy no-qop form when the challenge offers none).
fn digest_authorization(
    url: &str,
    is_post: bool,
    login: &str,
    pass: &str,
    ch: &DigestChallenge,
) -> Option<String> {
    let method = if is_post { "POST" } else { "GET" };
    let uri = request_uri(url);

    let ha1 = crate::util::md5_hex(format!("{}:{}:{}", login, ch.realm, pass));
    let ha2 = crate::util::md5_hex(format!("{method}:{uri}"));

    let (qop, nc, cnonce) = match ch.qop.as_deref() {
        None => (None, None, None),
        Some(q) if q.split(',').any(|x| x.trim() == "auth") => (
            Some("auth".to_string()),
            Some("00000001".to_string()),
            Some(crate::util::random_hex(8)),
        ),
        Some(_) => {
            // TODO(port): digest qop=auth-int unsupported (challenge offers no
            // "auth"); feed servers only ever advertise "auth".
            tracing::warn!("digest qop=auth-int unsupported (challenge offers no \"auth\")");
            return None;
        }
    };

    let response = match (&qop, &nc, &cnonce) {
        (Some(q), Some(nc), Some(cn)) => {
            crate::util::md5_hex(format!("{ha1}:{}:{nc}:{cn}:{q}:{ha2}", ch.nonce))
        }
        _ => crate::util::md5_hex(format!("{ha1}:{}:{ha2}", ch.nonce)),
    };

    let mut header = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
        login, ch.realm, ch.nonce, uri, response
    );
    if let (Some(q), Some(nc), Some(cn)) = (qop, nc, cnonce) {
        header.push_str(&format!(", qop={q}, nc={nc}, cnonce=\"{cn}\""));
    }
    if let Some(op) = &ch.opaque {
        header.push_str(&format!(", opaque=\"{op}\""));
    }
    Some(header)
}

/// Resolve the IP of a URL's host (the PHP's `gethostbyname` equivalent). An
/// IP-literal host is returned as-is; otherwise the first resolved address is
/// used, falling back to the host on failure. Runs the blocking lookup off the
/// async runtime.
async fn resolve_host_ip(url: &str) -> String {
    let parsed = match parse_url(url) {
        Some(p) => p,
        None => return String::new(),
    };
    let host = parsed.host.to_string();
    if host.is_empty() {
        return String::new();
    }
    let port: u16 = if parsed.scheme == "https" { 443 } else { 80 };

    tokio::task::spawn_blocking(move || {
        if host.parse::<std::net::Ipv4Addr>().is_ok() || host.parse::<std::net::Ipv6Addr>().is_ok()
        {
            return host;
        }
        (host.as_str(), port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .map(|a| a.ip().to_string())
            .unwrap_or(host)
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) must be unwrapped to plain IPv4
    /// before the range checks (PHP `inet_pton`/`inet_ntop` + `substr`).
    #[test]
    fn mapped_ipv4_unwrap() {
        let check = |url: &str| has_disallowed_ip(parse_url(url).as_ref().unwrap(), false);
        // Mapped PUBLIC → allowed (was the bug: 0000::/8 "zero bytes" rule).
        assert!(!check("http://[::ffff:8.8.8.8]/feed"));
        // Mapped loopback → rejected on any port.
        assert!(check("http://[::ffff:127.0.0.1]:8080/feed"));
        // Mapped RFC1918 → rejected on a non-standard port.
        assert!(check("http://[::ffff:192.168.1.1]:8080/feed"));
        // Plain forms unchanged.
        assert!(!check("http://8.8.8.8/feed"));
        assert!(check("http://192.168.1.1:8080/feed"));
        // Plain IPv6 unchanged.
        assert!(!check("http://[2001:db8::1]/feed"));
        assert!(check("http://[fe80::1]/feed"));
        assert!(check("http://[::1]/feed"));
    }

    #[test]
    fn user_agent_override() {
        // `TTRSS_HTTP_USER_AGENT` surface (gap-scan §F): unset/blank → the
        // PHP default; set → verbatim.
        let default = format!("Tiny Tiny RSS/{VERSION} (https://github.com/tt-rss/tt-rss)");
        assert_eq!(ua_for(None), default);
        assert_eq!(ua_for(Some("")), default);
        assert_eq!(ua_for(Some("   ")), default);
        assert_eq!(
            ua_for(Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"
        );
    }

    #[test]
    fn rewrite_relative_path() {
        assert_eq!(
            rewrite_relative("https://example.com/base/", "relative/path").as_deref(),
            Some("https://example.com/base/relative/path")
        );
        assert_eq!(
            rewrite_relative("https://cdn.example.com/", "images/photo.jpg").as_deref(),
            Some("https://cdn.example.com/images/photo.jpg")
        );
        assert_eq!(
            rewrite_relative("https://cdn.example.com/", "relative/audio.mp3").as_deref(),
            Some("https://cdn.example.com/relative/audio.mp3")
        );
    }

    #[test]
    fn rewrite_relative_absolute_passthrough() {
        assert_eq!(
            rewrite_relative("https://example.com/", "https://other.com/x").as_deref(),
            Some("https://other.com/x")
        );
    }

    #[test]
    fn php_dirname_cases() {
        assert_eq!(php_dirname("/base"), "");
        assert_eq!(php_dirname("/base/dir"), "/base");
        assert_eq!(php_dirname("base"), ".");
        assert_eq!(php_dirname("/"), "/");
        assert_eq!(php_dirname(""), ".");
    }

    // -- M6c.1 fetch tests -------------------------------------------------
    //
    // The fetch target URLs are non-loopback public IPs (8.8.8.8, 1.1.1.1), so
    // they pass the extended-URL validation gate without any DNS lookup and are
    // never actually contacted: reqwest routes them through a local `MockProxy`
    // (a loopback HTTP server standing in for the network). This mirrors the
    // PHP's `UrlHelperTest` mock-handler approach without needing a test double
    // wired into reqwest.

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A canned HTTP response served by [`MockProxy`].
    #[derive(Clone)]
    struct Canned {
        status: u16,
        reason: &'static str,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    /// A minimal mock HTTP server used as a proxy (see the note above). It
    /// serves canned responses in order, repeating the last when exhausted.
    struct MockProxy {
        port: u16,
        /// The raw request bytes, one entry per connection (in order).
        requests: Arc<Mutex<Vec<Vec<u8>>>>,
        _thread: std::thread::JoinHandle<()>,
    }

    impl MockProxy {
        fn new(responses: Vec<Canned>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let responses = Arc::new(Mutex::new(responses));
            let count = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let r = responses.clone();
            let c = count.clone();
            let reqs = requests.clone();
            let thread = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    // Read the request (its contents are irrelevant).
                    let mut data: Vec<u8> = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                data.extend_from_slice(&buf[..n]);
                                if data.windows(4).any(|w| w == b"\r\n\r\n") || data.len() > 65536 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    reqs.lock().unwrap().push(data);
                    let idx = c.fetch_add(1, Ordering::Relaxed);
                    let resp = {
                        let resps = r.lock().unwrap();
                        resps[idx.min(resps.len().saturating_sub(1))].clone()
                    };
                    let mut have_cl = false;
                    let mut s = format!("HTTP/1.1 {} {}\r\n", resp.status, resp.reason);
                    for (k, v) in &resp.headers {
                        s.push_str(&format!("{k}: {v}\r\n"));
                        if k.eq_ignore_ascii_case("content-length") {
                            have_cl = true;
                        }
                    }
                    if !have_cl {
                        s.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
                    }
                    s.push_str("Connection: close\r\n\r\n");
                    let mut out = s.into_bytes();
                    out.extend_from_slice(&resp.body);
                    let _ = stream.write_all(&out);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            Self {
                port,
                requests,
                _thread: thread,
            }
        }

        fn proxy(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        /// The i-th recorded raw request, as lossy text ("" if absent).
        fn request(&self, i: usize) -> String {
            self.requests
                .lock()
                .unwrap()
                .get(i)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        }
    }

    fn target() -> String {
        "http://8.8.8.8/feed".into()
    }

    fn opts(url: &str, proxy: String) -> FetchOptions {
        FetchOptions {
            url: url.into(),
            proxy: Some(proxy),
            followlocation: Some(false),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn fetch_returns_200_body() {
        let mock = MockProxy::new(vec![Canned {
            status: 200,
            reason: "OK",
            headers: vec![],
            body: b"the feed body".to_vec(),
        }]);
        let res = fetch(&opts(&target(), mock.proxy())).await;
        assert_eq!(res.body.as_deref(), Some(b"the feed body".as_slice()));
        assert_eq!(res.error_code, 200);
        assert_eq!(res.error, "HTTP/1.1 200 OK");
        assert_eq!(res.effective_url, "http://8.8.8.8/feed");
        assert_eq!(res.effective_ip, "8.8.8.8");
    }

    #[tokio::test]
    async fn fetch_empty_body_is_error() {
        let mock = MockProxy::new(vec![Canned {
            status: 200,
            reason: "OK",
            headers: vec![],
            body: Vec::new(),
        }]);
        let res = fetch(&opts(&target(), mock.proxy())).await;
        assert_eq!(res.body, None);
        assert_eq!(
            res.error,
            "Successful response, but no content was received."
        );
        assert_eq!(res.error_code, 200);
    }

    #[tokio::test]
    async fn fetch_403_retries_with_any_auth() {
        let mock = MockProxy::new(vec![
            Canned {
                status: 403,
                reason: "Forbidden",
                headers: vec![],
                body: Vec::new(),
            },
            Canned {
                status: 200,
                reason: "OK",
                headers: vec![],
                body: b"Retried OK".to_vec(),
            },
        ]);
        let mut o = opts(&target(), mock.proxy());
        o.login = Some("user".into());
        o.pass = Some("pass".into());
        o.auth_type = Some("basic".into());
        let res = fetch(&o).await;
        assert_eq!(res.body.as_deref(), Some(b"Retried OK".as_slice()));
        assert_eq!(res.error_code, 200);
    }

    #[tokio::test]
    async fn fetch_403_without_auth_does_not_retry() {
        let mock = MockProxy::new(vec![Canned {
            status: 403,
            reason: "Forbidden",
            headers: vec![],
            body: Vec::new(),
        }]);
        let res = fetch(&opts(&target(), mock.proxy())).await;
        assert_eq!(res.body, None);
        assert_eq!(res.error_code, 403);
    }

    /// A `401 + WWW-Authenticate: Digest` challenge with `auth_type = "digest"`
    ///: the first request carries no auth header; the resend carries the
    /// computed `Authorization: Digest` (verified against an independent MD5).
    #[tokio::test]
    async fn fetch_digest_challenge_round_trip() {
        let mock = MockProxy::new(vec![
            Canned {
                status: 401,
                reason: "Unauthorized",
                headers: vec![(
                    "WWW-Authenticate",
                    "Digest realm=\"ttrss\", nonce=\"abc123\", qop=\"auth\"".into(),
                )],
                body: Vec::new(),
            },
            Canned {
                status: 200,
                reason: "OK",
                headers: vec![],
                body: b"digest ok".to_vec(),
            },
        ]);
        let mut o = opts(&target(), mock.proxy());
        o.login = Some("user".into());
        o.pass = Some("pass".into());
        o.auth_type = Some("digest".into());
        let res = fetch(&o).await;
        assert_eq!(res.body.as_deref(), Some(b"digest ok".as_slice()));
        assert_eq!(res.error_code, 200);

        // First attempt: unauthenticated (digest is challenge-driven).
        assert!(
            !mock
                .request(0)
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        // Resend: the computed digest header.
        // reqwest writes header names lowercase on the wire.
        let req1 = mock.request(1);
        let auth = req1
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization: "))
            .unwrap();
        let header = auth.split_once(": ").unwrap().1;
        assert!(
            header.starts_with(
                "Digest username=\"user\", realm=\"ttrss\", nonce=\"abc123\", uri=\"/feed\", "
            ),
            "header: {header}"
        );
        let field = |name: &str| -> String {
            let quoted = matches!(name, "response" | "cnonce");
            let marker = if quoted {
                format!("{name}=\"")
            } else {
                format!("{name}=")
            };
            let Some(i) = header.find(&marker) else {
                panic!("missing {name} in: {header}")
            };
            let rest = &header[i + marker.len()..];
            if quoted {
                rest.split('"').next().unwrap().to_string()
            } else {
                rest.split(',').next().unwrap().trim().to_string()
            }
        };
        let response = field("response");
        let cnonce = field("cnonce");
        assert_eq!(field("qop"), "auth");
        assert_eq!(field("nc"), "00000001");
        let ha1 = crate::util::md5_hex("user:ttrss:pass");
        let ha2 = crate::util::md5_hex("GET:/feed");
        let expected = crate::util::md5_hex(format!("{ha1}:abc123:00000001:{cnonce}:auth:{ha2}"));
        assert_eq!(response, expected);
    }

    /// `auth_type = "basic"` + 403: the retry runs as "any" (preemptive basic)
    /// and its digest challenge round-trip completes the login.
    #[tokio::test]
    async fn fetch_403_retry_any_answers_digest_challenge() {
        let mock = MockProxy::new(vec![
            Canned {
                status: 403,
                reason: "Forbidden",
                headers: vec![],
                body: Vec::new(),
            },
            Canned {
                status: 401,
                reason: "Unauthorized",
                headers: vec![(
                    "WWW-Authenticate",
                    "Digest realm=\"ttrss\", nonce=\"n2\"".into(),
                )],
                body: Vec::new(),
            },
            Canned {
                status: 200,
                reason: "OK",
                headers: vec![],
                body: b"any ok".to_vec(),
            },
        ]);
        let mut o = opts(&target(), mock.proxy());
        o.login = Some("user".into());
        o.pass = Some("pass".into());
        o.auth_type = Some("basic".into());
        let res = fetch(&o).await;
        assert_eq!(res.body.as_deref(), Some(b"any ok".as_slice()));
        assert_eq!(res.error_code, 200);

        let b64 =
            base64::engine::Engine::encode(&base64::engine::general_purpose::STANDARD, "user:pass");
        // reqwest writes header names lowercase on the wire.
        // Attempt 1 (basic) and the "any" retry both send preemptive basic.
        assert!(
            mock.request(0)
                .contains(&format!("authorization: Basic {b64}"))
        );
        assert!(
            mock.request(1)
                .contains(&format!("authorization: Basic {b64}"))
        );
        // The retry's challenge round-trip answers with legacy (no-qop) digest.
        let req2 = mock.request(2);
        let auth = req2
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization: "))
            .unwrap();
        assert!(auth.contains("Digest username=\"user\""));
        let ha1 = crate::util::md5_hex("user:ttrss:pass");
        let ha2 = crate::util::md5_hex("GET:/feed");
        let expected = crate::util::md5_hex(format!("{ha1}:n2:{ha2}"));
        assert!(auth.contains(&format!("response=\"{expected}\"")));
        assert!(!auth.contains("qop="));
    }

    /// `auth_type = "basic"` does NOT answer a digest challenge (PHP: only
    /// `CURLAUTH_ANY`/digest do) — the 401 is returned as-is.
    #[tokio::test]
    async fn fetch_basic_scheme_ignores_digest_challenge() {
        let mock = MockProxy::new(vec![Canned {
            status: 401,
            reason: "Unauthorized",
            headers: vec![(
                "WWW-Authenticate",
                "Digest realm=\"ttrss\", nonce=\"abc123\", qop=\"auth\"".into(),
            )],
            body: Vec::new(),
        }]);
        let mut o = opts(&target(), mock.proxy());
        o.login = Some("user".into());
        o.pass = Some("pass".into());
        o.auth_type = Some("basic".into());
        let res = fetch(&o).await;
        assert_eq!(res.body, None);
        assert_eq!(res.error_code, 401);
        assert_eq!(
            mock.requests.lock().unwrap().len(),
            1,
            "no challenge resend"
        );
    }

    #[tokio::test]
    async fn fetch_redirect_to_loopback_rejected() {
        let mock = MockProxy::new(vec![Canned {
            status: 301,
            reason: "Moved Permanently",
            headers: vec![("Location", "http://127.0.0.1".into())],
            body: Vec::new(),
        }]);
        let mut o = opts(&target(), mock.proxy());
        o.followlocation = Some(true);
        let res = fetch(&o).await;
        assert_eq!(res.body, None);
        // reqwest normalizes the host-only Location to add a trailing slash
        // (the PHP records the raw Location header), so match by prefix.
        assert!(
            res.effective_url.starts_with("http://127.0.0.1"),
            "effective_url: {}",
            res.effective_url
        );
        assert!(
            res.error.contains("failed extended validation"),
            "error: {}",
            res.error
        );
    }

    #[tokio::test]
    async fn fetch_follows_valid_redirect() {
        let mock = MockProxy::new(vec![
            Canned {
                status: 301,
                reason: "Moved Permanently",
                headers: vec![("Location", "http://1.1.1.1/final".into())],
                body: Vec::new(),
            },
            Canned {
                status: 200,
                reason: "OK",
                headers: vec![],
                body: b"final body".to_vec(),
            },
        ]);
        let mut o = opts(&target(), mock.proxy());
        o.followlocation = Some(true);
        let res = fetch(&o).await;
        assert_eq!(res.body.as_deref(), Some(b"final body".as_slice()));
        assert_eq!(res.effective_url, "http://1.1.1.1/final");
        assert_eq!(res.error_code, 200);
    }

    #[tokio::test]
    async fn fetch_gzip_response_decompressed() {
        let original = b"gzip feed body";
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            enc.write_all(original).unwrap();
            enc.finish().unwrap();
        }
        let mock = MockProxy::new(vec![Canned {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Encoding", "gzip".into())],
            body: gz,
        }]);
        let res = fetch(&opts(&target(), mock.proxy())).await;
        assert_eq!(res.body.as_deref(), Some(original.as_slice()));
        assert_eq!(res.error_code, 200);
    }

    #[tokio::test]
    async fn fetch_304_not_modified_has_no_body() {
        let mock = MockProxy::new(vec![Canned {
            status: 304,
            reason: "Not Modified",
            headers: vec![("Last-Modified", "Mon, 01 Jan 2024 00:00:00 GMT".into())],
            body: Vec::new(),
        }]);
        let mut o = opts(&target(), mock.proxy());
        o.last_modified = Some("Sun, 01 Jan 2024 00:00:00 GMT".into());
        let res = fetch(&o).await;
        assert_eq!(res.body, None);
        assert_eq!(res.error_code, 304);
    }
}
