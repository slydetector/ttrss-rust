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
//   * reqwest has no `CURLAUTH_ANY` equivalent; the 403 retry (the PHP retries
//     once with `CURLAUTH_ANY`) is modeled as a single re-send — the
//     observable effect for the basic-auth feeds this port handles.
//   * Per-request timeouts default to the PHP's FILE_FETCH_TIMEOUT (45 s) and
//     FILE_FETCH_CONNECT_TIMEOUT (15 s); M6c.7 wires the `Config` surface.

/// `UrlHelper::get_user_agent()` — the User-Agent sent on fetches:
/// `Tiny Tiny RSS/{version} (https://github.com/tt-rss/tt-rss)`.
fn default_user_agent() -> String {
    format!("Tiny Tiny RSS/{VERSION} (https://github.com/tt-rss/tt-rss)")
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

    let auth_type = opts.auth_type.clone().unwrap_or_else(|| "basic".into());
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
    if let Some(proxy) = &opts.proxy {
        if let Ok(p) = reqwest::Proxy::all(proxy) {
            builder = builder.proxy(p);
        }
    }
    let Ok(client) = builder.build() else {
        res.error = "Failed to build HTTP client.".into();
        return res;
    };

    // Send (with the 403 auth retry — the PHP retries once with CURLAUTH_ANY).
    let resp = match send_once(&client, &url, opts, &auth_type, &useragent).await {
        Ok(r) => {
            let needs_retry = r.status() == reqwest::StatusCode::FORBIDDEN
                && opts
                    .login
                    .as_deref()
                    .map(|l| !l.is_empty())
                    .unwrap_or(false)
                && opts.pass.as_deref().map(|p| !p.is_empty()).unwrap_or(false)
                && auth_type != "any";
            if needs_retry {
                match send_once(&client, &url, opts, &auth_type, &useragent).await {
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

/// Build and send a single request for [`fetch`].
async fn send_once(
    client: &reqwest::Client,
    url: &str,
    opts: &FetchOptions,
    auth_type: &str,
    useragent: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let is_basic_or_digest = matches!(auth_type, "basic" | "digest");
    let is_post = opts.post_query.is_some();

    let mut req = if is_post {
        client.post(url)
    } else {
        client.get(url)
    };
    req = req.header("User-Agent", useragent);

    // Conditional request (skipped for POST, matching the PHP).
    if let Some(lm) = &opts.last_modified {
        if !is_post {
            req = req.header("If-Modified-Since", lm);
        }
    }
    if let Some(etag) = &opts.etag {
        if !is_post {
            req = req.header("If-None-Match", etag);
        }
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

    if is_basic_or_digest {
        if let (Some(login), Some(pass)) = (&opts.login, &opts.pass) {
            if !login.is_empty() && !pass.is_empty() {
                req = req.basic_auth(login, Some(pass));
            }
        }
    }

    if let Some(post) = &opts.post_query {
        req = req.form(post);
    }

    req.send().await
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
        _thread: std::thread::JoinHandle<()>,
    }

    impl MockProxy {
        fn new(responses: Vec<Canned>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let responses = Arc::new(Mutex::new(responses));
            let count = Arc::new(AtomicUsize::new(0));
            let r = responses.clone();
            let c = count.clone();
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
                _thread: thread,
            }
        }

        fn proxy(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
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
