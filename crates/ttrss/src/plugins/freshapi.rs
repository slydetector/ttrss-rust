//! The freshapi plugin (docs/22): the Google Reader / FreshRSS-compatible
//! API spoken by Reeder, NetNewsWire, FeedMe, Fluent Reader and friends.
//!
//! Spec: the freshapi tt-rss plugin (`freshapi/api/freshapi.php`, a
//! gitignored local clone). The port lands that logic *inside* the server
//! (plugin routes `/freshapi` and `/plugins.local/freshapi/api/greader.php`) and calls the port's
//! own API surface (`api::login`, `api::get_headlines`, `api::get_labels`,
//! `api::get_counters`, `api::update_article`, `api::get_article`) where
//! the reference loops back over HTTP (`callTinyTinyRssApi`).
//!
//! The full surface (phases 1+2 of docs/22): ClientLogin + session auth,
//! `stream/contents` (feed / label / category / reading-list / starred),
//! `stream/items/ids`, `stream/items/contents` (FeedMe), `edit-tag`,
//! `unread-count`, `tag/list` (json), `token`, `user-info`, and the
//! management verbs (`subscription/list|export|import|edit|quickadd`,
//! `rename-tag`, `disable-tag`, `mark-all-as-read`).

use async_trait::async_trait;
use axum::http::Method;
use serde_json::{Map, Value, json};
use sqlx::{AssertSqlSafe, QueryBuilder, Row};

use crate::api;
use crate::core::db::Db;
use crate::i18n;
use crate::labels::{self, LABEL_BASE_INDEX};
use crate::opml;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, MethodOutput, Plugin, RouteCtx, RouteRequest};
use crate::prefs;
use crate::session;
use crate::util;

/// `freshapi/init.php`'s `class FreshAPI`.
#[derive(Default)]
pub struct FreshApi;

/// `parse()`'s CORS block — on every response (freshapi.php:1463-1466).
const CORS: [(&str, &str); 4] = [
    ("access-control-allow-headers", "Authorization"),
    ("access-control-allow-methods", "GET, POST"),
    ("access-control-allow-origin", "*"),
    ("access-control-max-age", "600"),
];

fn cors() -> Vec<(String, String)> {
    CORS.map(|(k, v)| (k.to_string(), v.to_string())).to_vec()
}

fn text(status: u16, body: &str) -> MethodOutput {
    MethodOutput::Response {
        status,
        content_type: "text/plain; charset=UTF-8",
        body: body.to_string(),
        headers: cors(),
    }
}

/// `unauthorized()` — 401 + `Google-Bad-Token` (freshapi.php:134).
fn unauthorized() -> MethodOutput {
    let mut out = text(401, "Unauthorized!");
    if let MethodOutput::Response { headers, .. } = &mut out {
        headers.push(("google-bad-token".into(), "true".into()));
    }
    out
}

/// `apiNotEnabled()` (freshapi.php:167).
fn api_not_enabled() -> MethodOutput {
    text(
        503,
        "API access or the FreshAPI plugin is not enabled for this user in the TT-RSS Preferences!",
    )
}

/// `internalServerError()` (freshapi.php:143).
fn internal_error() -> MethodOutput {
    text(500, "Internal Server Error!")
}

/// A JSON endpoint's response: 200, `application/json; charset=UTF-8`,
/// `json_encode` + the trailing `"\n"` the reference echoes (every JSON
/// endpoint except `user-info`, which passes `false`).
fn json_result(value: &Value, trailing_newline: bool) -> MethodOutput {
    MethodOutput::Response {
        status: 200,
        content_type: "application/json; charset=UTF-8",
        body: if trailing_newline {
            format!("{}\n", value)
        } else {
            value.to_string()
        },
        headers: cors(),
    }
}

#[async_trait]
impl Plugin for FreshApi {
    fn name(&self) -> &'static str {
        "freshapi"
    }

    fn class_name(&self) -> &'static str {
        "FreshAPI"
    }

    fn about(&self) -> About {
        About {
            version: Some("1.2".into()),
            description: "A FreshRSS / Google Reader API Plugin for Tiny-Tiny RSS".into(),
            author: "Eric Pierce".into(),
            system: false,
            more_info: "https://github.com/eric-pierce/freshapi".into(),
        }
    }

    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        // docs/22 mount points: the native path, plus the exact URL a PHP
        // tt-rss + freshapi install serves (`plugins.local/freshapi/api/
        // greader.php`) — a migrated client keeps its base URL as-is.
        host.add_route("/freshapi", &[Method::GET, Method::POST, Method::OPTIONS]);
        host.add_route(
            "/plugins.local/freshapi/api/greader.php",
            &[Method::GET, Method::POST, Method::OPTIONS],
        );
    }

    async fn handle_route(
        &self,
        host: &mut HostCtx,
        route: &RouteCtx,
        req: &RouteRequest<'_>,
    ) -> MethodOutput {
        dispatch(host, route, req).await
    }
}

// ── Dispatch (`FreshGReaderAPI::parse`, freshapi.php:1460) ──────────────────

/// `$_REQUEST`: the query string plus the urlencoded body (body wins, PHP
/// `request_order` = "GP").
struct Params {
    req: std::collections::HashMap<String, String>,
    /// The raw body (`php://input`) — `multiplePosts` reads it, unmerged.
    body: String,
}

fn parse_request(query: &str, body: &[u8]) -> Params {
    let mut req: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let body = String::from_utf8_lossy(body).to_string();
    // `parse_str` (and so `$_REQUEST`): a bare key without `=` takes the
    // empty value (`?s` → `isset($_REQUEST['s'])` is true).
    for src in [query, &body] {
        for pair in src.split('&') {
            if pair.is_empty() {
                continue;
            }
            match pair.split_once('=') {
                Some((k, v)) => {
                    req.insert(urldecode(k), urldecode(v));
                }
                None => {
                    req.insert(urldecode(pair), String::new());
                }
            }
        }
    }
    Params { req, body }
}

/// `multiplePosts($name)` (freshapi.php:44): the values of every `name=`
/// pair in the **raw body** (the reference splits `php://input` on `&`
/// and prefix-matches).
fn multiple_posts(body: &str, name: &str) -> Vec<String> {
    let prefix = format!("{name}=");
    body.split('&')
        .filter_map(|p| p.strip_prefix(&prefix).map(urldecode))
        .collect()
}

/// Drop the `$N` placeholders from a condition (`push_bind` emits its
/// own, in bind-call order). A trailing placeholder (the reference scope
/// conditions end with one) is dropped whole.
fn strip_params(cond: &str) -> String {
    let mut out = String::with_capacity(cond.len());
    let mut rest = cond;
    while let Some(i) = rest.find('$') {
        let after = &rest[i + 1..];
        match after.find(|c: char| !c.is_ascii_digit()) {
            Some(0) => {
                // placeholder ends the string
                out.push_str(&rest[..i]);
                rest = "";
            }
            Some(j) => {
                out.push_str(&rest[..i]);
                rest = &rest[i + 1 + j..];
            }
            None => {
                out.push_str(&rest[..i]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// nginx's `merge_slashes on` (the nginx default) collapsed consecutive
/// slashes in the request path before PHP ever saw `PATH_INFO`; this
/// router does not, and News Flash 5.2.5 emits `api/greader.php//reader/…`
/// (news-flash 3.2.0 `get_instance` appends `/` to the stored
/// `…/greader.php/` base; greader_api 0.6 joins without normalizing).
/// Collapse here so dispatch sees the normalized path the reference did.
fn merge_slashes(s: &str) -> String {
    let mut prev_slash = false;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '/' && prev_slash {
            continue;
        }
        prev_slash = c == '/';
        out.push(c);
    }
    out
}

/// PHP `urldecode`: `%XX` + `+` → space. Lenient like PHP — valid
/// sequences decode, invalid ones (`%z`, a trailing `%`) stay literal.
/// Decoded bytes go through `from_utf8_lossy` (PHP keeps raw bytes; Rust
/// strings must be valid UTF-8).
fn urldecode(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `user/-/state|label/…` — some clients put their own user id in the
/// stream id (`user/1234/state/…`); normalize it (freshapi.php:899).
fn normalize_stream_id(stream_id: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^user/[^/]+/(state|label)/").unwrap())
        .replace(stream_id, "user/-/${1}/")
        .into_owned()
}

/// `hex2dec` (freshapi.php:95): 0 unless every byte is hex; 64-bit
/// `hexdec` otherwise.
fn hex2dec(hex: &str) -> i64 {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return 0;
    }
    i64::from_str_radix(hex, 16).unwrap_or(0)
}

/// `escapeToUnicodeAlternative` (freshapi.php:23): decode entities, then
/// the fullwidth forms of `&<>` — and of `?\/;` for titles (`extended`).
fn escape_to_unicode_alternative(text: &str, extended: bool) -> String {
    let text = util::html_entity_decode(text);
    let mut s = text
        .replace('&', "\u{FF06}")
        .replace('<', "\u{FF1C}")
        .replace('>', "\u{FF1E}");
    if extended {
        s = s
            .replace('?', "\u{FF1F}")
            .replace('\\', "\u{FF3C}")
            .replace('/', "\u{FF0F}")
            .replace(';', "\u{FF1B}");
    }
    s = util::php_trim(&s).to_string();
    s
}

/// `mb_strcut($content, 0, 500000, 'UTF-8')` — byte truncation on a char
/// boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `parse_url($link)` → `scheme://host` (no port) for `origin.htmlUrl`.
fn origin_url(link: &str) -> Option<String> {
    let (scheme, rest) = link.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

async fn dispatch(host: &mut HostCtx, route: &RouteCtx, req: &RouteRequest<'_>) -> MethodOutput {
    if req.method == Method::OPTIONS {
        return MethodOutput::Response {
            status: 204,
            content_type: "",
            body: String::new(),
            headers: cors(),
        };
    }

    // `urldecode($pathInfo)`, then the reference's
    // `preg_replace('%^(/api)?(/greader\.php)?%', '')`: clients address
    // the PHP mount as `…/api/greader.php/…`, so that prefix survives the
    // router's route-prefix strip and is discarded here. The router's
    // `path_info` has no leading slash; PHP's `PATH_INFO` does (its
    // `explode` yields a leading empty element — the indices below assume
    // that shape).
    let raw = urldecode(req.path_info);
    let raw = merge_slashes(&raw);
    let path = {
        let p = raw.strip_prefix("api/").unwrap_or(&raw);
        let p = p.strip_prefix("greader.php").unwrap_or(p);
        p.strip_prefix('/').unwrap_or(p).to_string()
    };
    let joined = format!("/{path}");
    let parts: Vec<&str> = joined.split('/').collect();
    let p = parse_request(req.query, req.body);

    if path.is_empty() && req.query.is_empty() {
        return text(200, "OK");
    }
    if parts.len() < 3 {
        return text(400, "Bad Request!");
    }

    if parts[1] == "accounts"
        && parts.get(2).copied() == Some("ClientLogin")
        && p.req.contains_key("Email")
        && p.req.contains_key("Passwd")
    {
        return client_login(route, &p.req["Email"], &p.req["Passwd"]).await;
    }

    if parts.get(1) == Some(&"reader")
        && parts.get(2) == Some(&"api")
        && parts.get(3) == Some(&"0")
        && parts.get(4).is_some()
    {
        let Some(authed) = authorized(host, route).await else {
            return unauthorized();
        };
        let (app, sess) = (&route.app, &route.sess);
        // `requireWriteAccess`: the session's access level is DB-validated
        // per request (`validate_session`), so no separate query.
        let readonly = authed.access_level == crate::core::user::ACCESS_LEVEL_READONLY;
        // `Accept-Language` for the user-locale translations (the
        // reference's `startup_gettext()` picks the session user's
        // locale).
        let accept_language = host
            .server_env
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Accept-Language"))
            .map(|(_, v)| v.as_str());
        let locale = i18n::resolve_user_locale(
            app,
            authed.uid,
            sess.data().profile().map(|p| p as i32),
            accept_language,
        )
        .await;

        // `stream` parameters (freshapi.php:1510-1532)
        let getp = |k: &str| p.req.get(k).map(|s| s.as_str()).unwrap_or("");
        let mut count = getp("n").parse::<i64>().ok().unwrap_or(20);
        if count < 1 {
            count = 20; // zero or negative values made the queries fail
        }
        if count > 10000 {
            count = 10000; // unbounded values let a single request run away
        }
        let order = getp("r");
        let start_time = getp("ot").parse::<i64>().ok().unwrap_or(0);
        let exclude_target = getp("xt");
        let continuation_raw = getp("c");
        let continuation: i64 = if !continuation_raw.is_empty()
            && continuation_raw.chars().all(|c| c.is_ascii_digit())
        {
            continuation_raw.parse().unwrap_or(0)
        } else {
            0
        };

        match parts[4] {
            "stream" => match parts.get(5).copied() {
                Some("contents") => {
                    // BazQux: a bare `/stream/contents?s=feed/…` appends
                    // the stream id segments to the path (freshapi.php:1534).
                    let mut eff: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
                    if eff.len() < 7 && !getp("s").is_empty() {
                        for seg in getp("s").split('/') {
                            eff.push(seg.to_string());
                        }
                    }
                    let scope = match (
                        eff.get(6).map(|s| s.as_str()),
                        eff.get(7).map(|s| s.as_str()),
                    ) {
                        (Some("feed"), Some(target)) => {
                            // A non-numeric target is a (URL-encoded) feed
                            // URL. The decoded path splits it on `/`, so the
                            // reference recovers it from the raw
                            // `REQUEST_URI` (the capture class stops at the
                            // first raw slash).
                            let target = if target.chars().all(|c| c.is_ascii_digit()) {
                                target.to_string()
                            } else {
                                let raw = req.path_info;
                                let marker = "stream/contents/feed/";
                                let rest = raw.split_once(marker).map(|(_, r)| r).unwrap_or("");
                                let m: String = rest
                                    .chars()
                                    .take_while(|c| {
                                        matches!(
                                            c,
                                            'a'..='z' | 'A'..='Z' | '0'..='9'
                                                | '\'' | '!' | '*' | '('
                                                | ')' | '%' | '$' | '_'
                                                | '.' | '~' | '+' | '-'
                                        )
                                    })
                                    .collect();
                                urldecode(&m)
                            };
                            Scope::Feed(target)
                        }
                        (Some("user"), Some(_))
                            if eff.get(8).map(|s| s.as_str()) == Some("state")
                                && eff.get(9).map(|s| s.as_str()) == Some("com.google") =>
                        {
                            match eff.get(10).map(|s| s.as_str()) {
                                Some("reading-list") => Scope::ReadingList,
                                Some("starred") => Scope::Starred,
                                _ => return text(400, "Bad Request!"),
                            }
                        }
                        (Some("user"), Some(_))
                            if eff.get(8).map(|s| s.as_str()) == Some("label") =>
                        {
                            Scope::Label(eff[9].clone())
                        }
                        // EasyRSS / FeedMe: bare `/stream/contents`
                        _ => Scope::ReadingList,
                    };
                    stream_contents(
                        host,
                        route,
                        &authed,
                        scope,
                        count,
                        order,
                        continuation,
                        start_time,
                    )
                    .await
                }
                Some("items") => match (parts.get(6).copied(), p.req.contains_key("s")) {
                    (Some("ids"), true) => {
                        stream_items_ids(
                            host,
                            route,
                            &authed,
                            &p.req["s"],
                            start_time,
                            count,
                            order,
                            exclude_target,
                            continuation,
                        )
                        .await
                    }
                    (Some("contents"), _) if p.req.contains_key("i") => {
                        // The reference's `multiplePosts` reads the raw POST
                        // body (`php://input`) — a GET `?i=…` guard matches,
                        // but the ids come from the body.
                        stream_items(host, route, &authed, &multiple_posts(&p.body, "i"), order)
                            .await
                    }
                    _ => text(400, "Bad Request!"),
                },
                _ => text(400, "Bad Request!"),
            },
            "tag" => {
                if parts.get(5).copied() == Some("list") {
                    if getp("output") != "json" {
                        return text(501, "Not Implemented!");
                    }
                    tag_list(host, route, &locale, &authed).await
                } else {
                    text(400, "Bad Request!")
                }
            }
            "unread-count" => {
                if getp("output") != "json" {
                    return text(501, "Not Implemented!");
                }
                unread_count(host, route, &locale, &authed).await
            }
            "edit-tag" => {
                edit_tag(
                    host,
                    route,
                    &authed,
                    &multiple_posts(&p.body, "i"),
                    getp("a"),
                    getp("r"),
                )
                .await
            }
            "token" => token(host, &authed).await,
            "user-info" => user_info(sess),
            "subscription" => match parts.get(5).copied() {
                Some("export") => subscription_export(app, sess).await,
                Some("import") => {
                    if readonly {
                        return forbidden();
                    }
                    // `POST && $ORIGINAL_INPUT != ''`
                    if req.method == Method::POST && !p.body.is_empty() {
                        subscription_import(app, sess, &p.body).await
                    } else {
                        text(400, "Bad Request!")
                    }
                }
                Some("list") => {
                    if getp("output") != "json" {
                        return text(501, "Not Implemented!");
                    }
                    subscription_list(host, route, &locale, &authed).await
                }
                Some("edit") => {
                    // `isset($ORIG_REQUEST['s'], $ORIG_REQUEST['ac'])`
                    if !p.req.contains_key("s") || !p.req.contains_key("ac") {
                        return text(400, "Bad Request!");
                    }
                    if readonly {
                        return forbidden();
                    }
                    // `empty($input['s']) && isset(...)` → the scalar form
                    // (an empty `s=`), else the raw-body `multiplePosts`.
                    let scalar = |k: &str| {
                        p.req
                            .get(k)
                            .map(|v| v.is_empty() || v == "0")
                            .unwrap_or(false)
                    };
                    // DEV: the reference reads `s`/`t` only from the body, so
                    // clients that put them in the query string (News Flash
                    // `subscription_edit`) get a silent no-op there. FreshRSS
                    // accepts the query form; we do too (body wins when both
                    // are present).
                    let query_fallback = |k: &str, body_vals: Vec<String>| -> Vec<String> {
                        if !body_vals.is_empty() {
                            body_vals
                        } else if let Some(q) = p.req.get(k) {
                            vec![q.clone()]
                        } else {
                            Vec::new()
                        }
                    };
                    let stream_names = if scalar("s") {
                        vec![String::new()]
                    } else {
                        query_fallback("s", multiple_posts(&p.body, "s"))
                    };
                    let titles = if scalar("t") {
                        vec![String::new()]
                    } else {
                        query_fallback("t", multiple_posts(&p.body, "t"))
                    };
                    subscription_edit(
                        host,
                        route,
                        &locale,
                        &authed,
                        &stream_names,
                        &titles,
                        &p.req["ac"],
                        p.req.get("a").map(|s| s.as_str()).unwrap_or(""),
                        p.req.get("r").map(|s| s.as_str()).unwrap_or(""),
                    )
                    .await
                }
                Some("quickadd") => {
                    if !p.req.contains_key("quickadd") {
                        return text(400, "Bad Request!");
                    }
                    if readonly {
                        return forbidden();
                    }
                    quickadd(host, route, &authed, &p.req["quickadd"]).await
                }
                _ => text(400, "Bad Request!"),
            },
            "rename-tag" => {
                if readonly {
                    return forbidden();
                }
                rename_tag(host, route, &locale, &authed, getp("s"), getp("dest")).await
            }
            "disable-tag" => {
                if readonly {
                    return forbidden();
                }
                // `foreach ($s_s as $s) { self::disableTag($s, …) }` — every
                // path exits, so only the first `s=` is processed.
                let s_list = multiple_posts(&p.body, "s");
                if s_list.is_empty() {
                    text(400, "Bad Request!")
                } else {
                    disable_tag(host, route, &locale, &authed, &s_list[0]).await
                }
            }
            "mark-all-as-read" => {
                let ts = p.req.get("ts").map(|t| t.trim()).unwrap_or("0");
                // `ctype_digit`
                if ts.is_empty() || !ts.chars().all(|c| c.is_ascii_digit()) {
                    return text(400, "Bad Request!");
                }
                mark_all_as_read(host, route, &locale, &authed, getp("s"), ts).await
            }
            _ => text(400, "Bad Request!"),
        }
    } else {
        text(400, "Bad Request!")
    }
}

// ── Auth ────────────────────────────────────────────────────────────────────

/// The authenticated caller (the reference's `authorizationToUser` returns
/// the session id; the port carries the bits the endpoints need).
struct Authed {
    uid: i32,
    // Phase 2 (the subscription/tag-management verbs) reads these.
    #[allow(dead_code)]
    user: String,
    sid: String,
    #[allow(dead_code)]
    access_level: i32,
}

/// `greader.php`'s session resume + `freshapiSessionAllowed`
/// (freshapi.php:72): the `Authorization: GoogleLogin auth=<user>/<sid>`
/// header names an existing session owned by `<user>`; the session must
/// validate (password + access level unchanged, account enabled), the
/// user must allow API access, and freshapi must be in the user's
/// `_ENABLED_PLUGINS`.
/// `headerVariable('Authorization', 'GoogleLogin_auth')` (freshapi.php:4,
/// 220): the header runs through `parse_str`, which turns spaces in keys
/// into underscores — so the standard GR form `GoogleLogin
/// auth=<user>/<sid>` and the literal `GoogleLogin_auth=<user>/<sid>`
/// both resolve; the slash form (`GoogleLogin_auth/user/sid`) does not.
fn authorization_value(host: &HostCtx) -> Option<String> {
    let header = host
        .server_env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Authorization"))?
        .1
        .as_str();
    for pair in header.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.replace([' ', '+'], "_") == "GoogleLogin_auth" {
            return Some(urldecode(v));
        }
    }
    None
}

async fn authorized(host: &HostCtx, route: &RouteCtx) -> Option<Authed> {
    let (app, sess) = (&route.app, &route.sess);
    let rest = authorization_value(host)?;
    let (user, sid) = rest.split_once('/')?;
    // `greader.php`'s resume guard (`^[a-zA-Z0-9,-]{1,128}$`).
    if !(1..=128).contains(&sid.len())
        || !sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b',')
    {
        return None;
    }
    let data = session::load(
        &Db::from_pool(host.pool().clone()),
        app.config.encryption_key,
        sid,
    )
    .await;
    if !data.is_logged_in() {
        return None;
    }
    // The token's username must belong to the session
    // (`SINGLE_USER_MODE` skips the check).
    if !app.config.single_user_mode
        && !data
            .name()
            .map(|n| n.eq_ignore_ascii_case(user))
            .unwrap_or(false)
    {
        return None;
    }
    // Adopt the session (`session_id($session_id); session_start();`).
    sess.replace(sid.to_string(), data);

    // `freshapiSessionAllowed` (freshapi.php:72) delegates the session
    // checks to `Sessions::validate_session` — the same shared function the
    // API surface uses (stale pwd hash / access level / disabled account),
    // incl. the `HOOK_VALIDATE_SESSION` chain.
    let uid = sess.data().uid().unwrap_or(0);
    let mut data = sess.data();
    if !session::validate_session(app, app.config.single_user_mode, &mut data).await {
        return None;
    }
    let profile = sess.data().profile().map(|p| p as i32);
    let rows = prefs::load_rows(&app.db, uid, profile).await;
    if !prefs::resolve("ENABLE_API_ACCESS", &rows, profile).as_bool() {
        return None;
    }
    // `freshapiEnabledForUser()`: the plugin is in the user's enabled set.
    if !plugin_enabled(&rows, profile) {
        return None;
    }
    Some(Authed {
        uid,
        user: user.to_string(),
        sid: sid.to_string(),
        access_level: sess.data().access_level().unwrap_or(0) as i32,
    })
}

fn plugin_enabled(rows: &[(String, Option<i32>, String)], profile: Option<i32>) -> bool {
    prefs::resolve(prefs::_ENABLED_PLUGINS, rows, profile)
        .as_str()
        .split(',')
        .any(|s| s.trim().eq_ignore_ascii_case("freshapi"))
}

/// `clientLogin` (freshapi.php:238): a real tt-rss `login` (app passwords,
/// OTP, the API-access gate), then the GR reply `SID=user/sid` / `LSID=` /
/// `Auth=user/sid`.
async fn client_login(route: &RouteCtx, email: &str, password: &str) -> MethodOutput {
    let (app, sess) = (&route.app, &route.sess);
    let mut login_body = Map::new();
    login_body.insert("user".into(), json!(email));
    login_body.insert("password".into(), json!(password));
    let (status, content) = api::login(app, sess, &login_body).await;
    if status != api::STATUS_OK {
        return unauthorized();
    }
    // `freshapiEnabledForUser()` after the login (the reference re-checks
    // the per-user plugin set once the session exists).
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let rows = prefs::load_rows(&app.db, uid, profile).await;
    let session_id = content
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !plugin_enabled(&rows, profile) {
        // `session_destroy(); self::apiNotEnabled();`
        let _ = session::destroy(app.db.pool(), &session_id).await;
        return api_not_enabled();
    }
    let data = sess.data();
    let name = data.name().unwrap_or(email);
    let auth = format!("{name}/{session_id}");
    let mut out = text(200, &format!("SID={auth}\nLSID=\nAuth={auth}\n"));
    if let MethodOutput::Response { headers, .. } = &mut out {
        headers.push(("cache-control".into(), "no-store".into()));
    }
    out
}

// ── Streams ─────────────────────────────────────────────────────────────────

/// The scope `stream/contents` was asked for (freshapi's `streamContents`
/// `$path`/`$streamId` pair).
#[derive(Debug, Clone)]
enum Scope {
    Feed(String),
    Label(String),
    ReadingList,
    Starred,
}

/// `Feeds::feedIdByUrl` (freshapi.php:515): the owner's feed with that
/// `feed_url`, 0 when absent.
async fn feed_id_by_url(host: &HostCtx, authed: &Authed, url: &str) -> i32 {
    sqlx::query("SELECT id FROM ttrss_feeds WHERE feed_url = $1 AND owner_uid = $2")
        .bind(url)
        .bind(authed.uid)
        .fetch_optional(host.pool())
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<i32, _>("id").ok())
        .unwrap_or(0)
}

/// `categoriesResponse` (freshapi.php:311): the owner's DB categories,
/// then the special `Labels` / `Special` / `Uncategorized` entries (in
/// that order — the reference's literal).
async fn categories(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
) -> Vec<(i32, String)> {
    let rows = sqlx::query("SELECT id, title FROM ttrss_feed_categories WHERE owner_uid = $1")
        .bind(authed.uid)
        .fetch_all(host.pool())
        .await
        .unwrap_or_default();
    let mut out: Vec<(i32, String)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                r.try_get::<i32, _>("id").ok()?,
                r.try_get::<String, _>("title").ok()?,
            ))
        })
        .collect();
    // `Feeds::_get_cat_title` (translated to the user's locale).
    let app: &crate::AppState = &route.app;
    let cache: &i18n::I18nCache = &app.i18n;
    out.push((
        crate::cats::CAT_LABELS,
        i18n::translate(locale.as_deref(), cache, "Labels"),
    ));
    out.push((
        crate::cats::CAT_SPECIAL,
        i18n::translate(locale.as_deref(), cache, "Special"),
    ));
    out.push((
        crate::cats::CAT_UNCATEGORIZED,
        i18n::translate(locale.as_deref(), cache, "Uncategorized"),
    ));
    out
}

/// `nameMatches` (freshapi.php:89): exact, or equal after HTML-decoding
/// (older TT-RSS versions stored escaped names).
fn name_matches(stored: &str, wanted: &str) -> bool {
    stored == wanted || util::html_entity_decode(stored) == wanted
}

/// `getCategoryLabelID` (freshapi.php:930): categories first (a category
/// always wins a label with the same name), then the user's labels.
async fn category_label_id(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    name: &str,
) -> Option<i32> {
    for (id, title) in categories(host, route, locale, authed).await {
        if name_matches(&title, name) {
            return Some(id);
        }
    }
    let (status, content) = api::get_labels(&route.app, Some(authed.uid), &Map::new()).await;
    if status == api::STATUS_OK
        && let Some(list) = content.as_array()
    {
        for l in list {
            if let Some(caption) = l.get("caption").and_then(Value::as_str)
                && name_matches(caption, name)
            {
                return l.get("id").and_then(Value::as_i64).map(|v| v as i32);
            }
        }
    }
    None
}

/// `streamScope` (freshapi.php:907): SQL conditions (on `ttrss_user_entries`
/// aliased `a`) for a `feed/` / `user/-/label/` stream, `None` when the
/// stream does not exist.
async fn stream_scope(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    stream_id: &str,
) -> Option<(String, Vec<i32>)> {
    if let Some(feed) = stream_id.strip_prefix("feed/") {
        let feed_id = if feed.chars().all(|c| c.is_ascii_digit()) {
            feed.parse::<i32>().unwrap_or(0)
        } else {
            feed_id_by_url(host, authed, feed).await
        };
        return if feed_id > 0 {
            Some(("a.feed_id = $1".to_string(), vec![feed_id]))
        } else {
            None
        };
    }
    if let Some(name) = stream_id.strip_prefix("user/-/label/") {
        let id = category_label_id(host, route, locale, authed, name).await?;
        if id == crate::cats::CAT_UNCATEGORIZED {
            return Some((
                "a.feed_id IN (SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND cat_id IS NULL)"
                    .to_string(),
                vec![authed.uid],
            ));
        }
        if id > 0 {
            return Some((
                "a.feed_id IN (SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND cat_id = $2)"
                    .to_string(),
                vec![authed.uid, id],
            ));
        }
        if id < LABEL_BASE_INDEX {
            return Some((
                "a.ref_id IN (SELECT article_id FROM ttrss_user_labels2 WHERE label_id = $1)"
                    .to_string(),
                vec![labels::feed_to_label_id(id)],
            ));
        }
    }
    None // unknown stream, or the virtual Special/Labels categories
}

/// `convertTtrssArticleToGreaderFormat` (freshapi.php:1128).
async fn convert_article(host: &HostCtx, article: &Value) -> Value {
    // `articleTimestamp` (freshapi.php:1118): the reference casts the
    // *formatted* `updated` string to int (PHP `(int)` semantics — the
    // leading digits), falling back to `date_entered`, then `time()`.
    let updated_raw = article.get("updated").and_then(Value::as_str).unwrap_or("");
    let mut ts = util::php_int_cast(updated_raw);
    if ts <= 0 {
        let id = article.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
        ts = sqlx::query_scalar::<_, i64>(
            "SELECT extract(epoch FROM date_entered)::bigint FROM ttrss_entries WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(host.pool())
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
        if ts <= 0 {
            ts = chrono::Utc::now().timestamp();
        }
    }

    let link = util::html_entity_decode(article.get("link").and_then(Value::as_str).unwrap_or(""));
    let title = article
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let feed_title = article
        .get("feed_title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let feed_id = article.get("feed_id").and_then(Value::as_i64).unwrap_or(0);
    let author = article.get("author").and_then(Value::as_str).unwrap_or("");
    let content = article.get("content").and_then(Value::as_str).unwrap_or("");
    let unread = article
        .get("unread")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let marked = article
        .get("marked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let aid = article.get("id").and_then(Value::as_i64).unwrap_or(0) as u64;

    let mut v = json!({
        // `dec2hex`: the hex id is zero-padded to 16 chars.
        "id": format!("tag:google.com,2005:reader/item/{aid:016x}"),
        "crawlTimeMsec": format!("{ts}000"),
        "timestampUsec": format!("{ts}000000"),
        "published": ts,
        "title": escape_to_unicode_alternative(title, true),
        "canonical": [{ "href": &link }],
        "alternate": [{ "href": &link }],
        "categories": [
            if unread { "user/-/state/com.google/reading-list" } else { "user/-/state/com.google/read" },
        ],
        "origin": {
            "streamId": format!("feed/{feed_id}"),
            "title": escape_to_unicode_alternative(feed_title, true),
        },
        "summary": { "content": truncate_utf8(content, 500000) },
        "author": author,
    });
    if marked {
        v["categories"]
            .as_array_mut()
            .unwrap()
            .push(json!("user/-/state/com.google/starred"));
    }
    if let Some(site_url) = origin_url(&link) {
        v["origin"]["htmlUrl"] = json!(util::html_entity_decode(&site_url));
    }
    if let Some(list) = article.get("labels").and_then(Value::as_array) {
        for l in list {
            if l.get(0).is_some()
                && let Some(caption) = l.get(1).and_then(Value::as_str)
            {
                v["categories"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(format!("user/-/label/{caption}")));
            }
        }
    }
    if let Some(atts) = article.get("attachments").and_then(Value::as_array)
        && !atts.is_empty()
    {
        let mut enclosure: Vec<Value> = Vec::new();
        for e in atts {
            let href = e.get("content_url").and_then(Value::as_str).unwrap_or("");
            let ctype = e.get("content_type").and_then(Value::as_str).unwrap_or("");
            // `!empty($duration) && intval($duration) != 0` →
            // `'length' => $duration` — the RAW value: numeric strings
            // encode as JSON numbers, anything else stays a string
            // (`"05:30"` → `"05:30"`, not `5`).
            let length = match e.get("duration") {
                Some(Value::Number(n)) if n.as_i64().unwrap_or(0) != 0 => {
                    Some(json!(n.as_i64().unwrap_or(0)))
                }
                Some(Value::String(s)) if !s.is_empty() && util::php_int_cast(s) != 0 => {
                    match s.parse::<i64>() {
                        Ok(v) => Some(json!(v)),
                        Err(_) => Some(json!(s)),
                    }
                }
                _ => None,
            };
            let mut item = Map::new();
            item.insert("href".into(), json!(href));
            item.insert("type".into(), json!(ctype));
            if let Some(l) = length {
                item.insert("length".into(), l);
            }
            enclosure.push(Value::Object(item));
        }
        v["enclosure"] = Value::Array(enclosure);
    }
    v
}

/// `streamContents` (freshapi.php:1026): `getHeadlines` scoped by
/// `feed_id`/`is_cat`/`view_mode`, 200 per page, `continuation` = the
/// running skip offset.
#[allow(clippy::too_many_arguments)] // mirrors `streamContents`'s parameter list
async fn stream_contents(
    host: &HostCtx,
    route: &RouteCtx,
    authed: &Authed,
    scope: Scope,
    count: i64,
    order: &str,
    continuation: i64,
    start_time: i64,
) -> MethodOutput {
    let locale = user_locale(&route.app, &route.sess, authed).await;
    let (feed_id, is_cat, view_mode, result_id) = match &scope {
        Scope::Feed(t) => {
            let feed_id: i64 = if t.chars().all(|c| c.is_ascii_digit()) {
                t.parse().unwrap_or(0)
            } else {
                feed_id_by_url(host, authed, t).await as i64
            };
            if feed_id <= 0 {
                // unknown feed: an empty stream rather than every article
                return stream_empty(&format!("feed/{t}"), continuation);
            }
            (feed_id, false, "unread", format!("feed/{t}"))
        }
        Scope::Label(name) => {
            let id = match category_label_id(host, route, &locale, authed, name).await {
                Some(id) => id,
                None => return stream_empty(&format!("user/-/label/{name}"), continuation),
            };
            if id > -10 {
                // below -10 are labels; above (and 0) are categories
                (id as i64, true, "unread", format!("user/-/label/{name}"))
            } else {
                (id as i64, false, "unread", format!("user/-/label/{name}"))
            }
        }
        Scope::Starred => (
            -4,
            false,
            "marked",
            "user/-/state/com.google/starred".to_string(),
        ),
        Scope::ReadingList => (
            -4,
            false,
            "unread",
            "user/-/state/com.google/reading-list".to_string(),
        ),
    };

    let mut item_refs: Vec<Value> = Vec::new();
    let mut skip = continuation;
    let mut more_available = false;
    while (item_refs.len() as i64) < count && (item_refs.len() as i64) <= 15000 {
        // `feed_id` -4 (CAT_ALL) = all articles by default.
        let mut body = Map::new();
        body.insert("limit".into(), json!(if count > 0 { count } else { 0 }));
        body.insert("skip".into(), json!(skip));
        body.insert("view_mode".into(), json!(view_mode));
        body.insert("feed_id".into(), json!(feed_id));
        body.insert("is_cat".into(), json!(is_cat));
        body.insert(
            "order_by".into(),
            json!(if order == "o" {
                "date_reverse"
            } else {
                "feed_dates"
            }),
        );
        body.insert("show_content".into(), json!(true));
        body.insert("include_attachments".into(), json!(true));
        // greader protocol: `ot` is "the timestamp of the oldest item that
        // you're interested in" (mihaip spec; the 2009 Google spec's
        // delta-sync pattern). Clients (News Flash) send `ot=<last sync>`
        // on every page, so the server-side filter is what makes re-syncs
        // cheap. Filter on `date_entered` (crawl time, per Arowser: "only
        // items crawled after this time"), not the published date — a
        // backdated item fetched today must reach the client. The reference
        // parses `ot` into `$min_date` and drops it (a regression from its
        // pre-`getHeadlines` rewrite, which filtered on it): deviation.
        // NOTE(protocol): `nt` (newest-of-interest) is read and dropped,
        // like the reference — no `end_ts` in the headline query; no known
        // client sends it.
        if start_time > 0 {
            let dt = chrono::DateTime::from_timestamp(start_time, 0)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH);
            body.insert(
                "start_ts".into(),
                json!(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
            );
        }
        let (status, content) = api::get_headlines(&route.app, &route.sess, &body, None).await;
        if status != api::STATUS_OK {
            return internal_error();
        }
        let items = content.as_array().cloned().unwrap_or_default();
        for a in &items {
            if (item_refs.len() as i64) < count {
                item_refs.push(convert_article(host, a).await);
            } else {
                more_available = true;
                break;
            }
        }
        if items.len() < 200 {
            break; // end of available items
        }
        skip += items.len() as i64;
    }

    let mut result = json!({
        "id": result_id,
        "updated": chrono::Utc::now().timestamp(),
        "items": item_refs,
    });
    // The reference: `'' . $offset + $totalFetched` (stringified).
    if more_available || (item_refs.len() as i64) == count {
        result["continuation"] = json!((continuation + item_refs.len() as i64).to_string());
    }
    json_result(&result, true)
}

/// An unknown feed/label answers an empty stream — but with a
/// `continuation` (freshapi's `totalFetched == $count` when `count = 0`).
fn stream_empty(result_id: &str, continuation: i64) -> MethodOutput {
    let result = json!({
        "id": result_id,
        "updated": chrono::Utc::now().timestamp(),
        "items": [],
        "continuation": continuation.to_string(),
    });
    json_result(&result, true)
}

/// The session user's locale (the reference's `startup_gettext`).
async fn user_locale(
    app: &crate::AppState,
    sess: &session::Sess,
    authed: &Authed,
) -> Option<String> {
    i18n::resolve_user_locale(
        app,
        authed.uid,
        sess.data().profile().map(|p| p as i32),
        None,
    )
    .await
}

/// `streamContentsItemsIds` (freshapi.php:818): the item ids of a stream,
/// one keyset-paged query on `ttrss_user_entries`.
#[allow(clippy::too_many_arguments)] // mirrors `streamContentsItemsIds`'s list
async fn stream_items_ids(
    host: &HostCtx,
    route: &RouteCtx,
    authed: &Authed,
    stream_id: &str,
    start_time: i64,
    count: i64,
    order: &str,
    exclude_target: &str,
    continuation: i64,
) -> MethodOutput {
    let pool = host.pool();
    let locale = user_locale(&route.app, &route.sess, authed).await;
    let stream_id = normalize_stream_id(stream_id);

    // The query is assembled with `QueryBuilder::push_bind`, which emits
    // the `$N` placeholders itself — in bind-call order. The scope
    // conditions therefore carry no placeholders of their own.
    let mut conds: Vec<(String, Vec<i32>)> = Vec::new();
    let mut read_only = false;
    let mut exists = true;
    match stream_id.as_str() {
        "user/-/state/com.google/read" => read_only = true,
        "user/-/state/com.google/starred" => {
            conds.push(("a.marked = true".to_string(), Vec::new()))
        }
        "user/-/state/com.google/reading-list" => {}
        _ => match stream_scope(host, route, &locale, authed, &stream_id).await {
            None => exists = false,
            Some((cond, cb)) => conds.push((strip_params(&cond), cb)),
        },
    }
    if exclude_target == "user/-/state/com.google/unread" {
        read_only = true;
    }
    let (ts_cond, ts_cond_tz, join) = if read_only {
        // "read since ot" is based on when the article was marked read
        conds.push(("a.unread = false".to_string(), Vec::new()));
        ("a.last_read >= (to_timestamp(", "))", String::new())
    } else {
        if exclude_target == "user/-/state/com.google/read" {
            conds.push(("a.unread = true".to_string(), Vec::new()));
        }
        (
            "b.date_entered >= (to_timestamp(",
            "))",
            " INNER JOIN ttrss_entries b ON a.ref_id = b.id".to_string(),
        )
    };
    let ascending = order == "o";
    let cont_cond: Option<&str> = if continuation > 0 {
        Some(if ascending {
            "a.ref_id > "
        } else {
            "a.ref_id < "
        })
    } else {
        None
    };

    if exists {
        let mut qb = QueryBuilder::<sqlx::Postgres>::new(String::new());
        qb.push("SELECT a.ref_id::varchar AS id FROM ttrss_user_entries a");
        qb.push(&join);
        qb.push(" WHERE a.owner_uid = ");
        qb.push_bind(authed.uid);
        for (c, binds) in &conds {
            qb.push(" AND ").push(c);
            for b in binds {
                qb.push_bind(*b);
            }
        }
        qb.push(" AND ").push(ts_cond);
        qb.push_bind(start_time as i32);
        qb.push(ts_cond_tz);
        if let Some(c) = cont_cond {
            qb.push(" AND ").push(c);
            qb.push_bind(continuation as i32);
        }
        qb.push(" ORDER BY a.ref_id ");
        qb.push(if ascending { "ASC" } else { "DESC" });
        qb.push(" LIMIT ");
        qb.push_bind(count + 1); // one extra row tells us whether a page exists
        let rows = match qb.build().fetch_all(pool).await {
            Ok(rows) => rows,
            Err(_) => return text(400, "Bad Request!"), // the reference's PDO catch
        };
        let items: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String, _>("id").ok())
            .collect();
        let mut item_refs: Vec<Value> = items.into_iter().map(|id| json!({ "id": id })).collect();
        let mut result = Map::new();
        if item_refs.len() as i64 > count {
            item_refs.pop();
            if let Some(last) = item_refs
                .last()
                .and_then(|v| v.get("id").and_then(Value::as_str))
            {
                result.insert("continuation".into(), json!(last));
            }
        }
        result.insert("itemRefs".into(), Value::Array(item_refs));
        return json_result(&Value::Object(result), true);
    }
    json_result(&json!({ "itemRefs": [] }), true)
}

/// `streamContentsItems` (freshapi.php:953): item contents for explicit
/// ids (FeedMe's `stream/items/contents?i=…`).
async fn stream_items(
    host: &HostCtx,
    route: &RouteCtx,
    authed: &Authed,
    e_ids: &[String],
    order: &str,
) -> MethodOutput {
    let pool = host.pool();
    // Feed → category — the category is added to the item's labels (and
    // becomes the title fallback for vfeed items).
    let locale = user_locale(&route.app, &route.sess, authed).await;
    let cat_titles: std::collections::HashMap<i32, String> =
        categories(host, route, &locale, authed)
            .await
            .into_iter()
            .collect();
    let feed_rows = sqlx::query(
        "SELECT id, COALESCE(cat_id, 0) AS cat_id FROM ttrss_feeds WHERE owner_uid = $1",
    )
    .bind(authed.uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut feed_cat: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
    for r in &feed_rows {
        if let (Ok(id), Ok(cat)) = (r.try_get::<i32, _>("id"), r.try_get::<i32, _>("cat_id")) {
            feed_cat.insert(id, cat);
        }
    }

    // `i` values: decimal ids, except hex (non-numeric, leading `0`, or
    // NetNewsWire — which hex-encodes ids of its own).
    let is_nnn = host
        .server_env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
        .map(|(_, v)| v.starts_with("NetNewsWire"))
        .unwrap_or(false);
    let mut decoded: Vec<i64> = Vec::new();
    for e_id in e_ids {
        let bare = e_id.rsplit('/').next().unwrap_or(e_id);
        let is_dec =
            !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit()) && !bare.starts_with('0');
        decoded.push(if is_dec && !is_nnn {
            bare.parse().unwrap_or(0)
        } else {
            hex2dec(bare)
        });
    }

    let mut body = Map::new();
    body.insert(
        "article_id".into(),
        json!(
            decoded
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    let (status, content) =
        api::get_article(&route.app, &route.sess, Some(authed.uid), &body).await;
    let mut items: Vec<Value> = Vec::new();
    if status == api::STATUS_OK
        && let Some(articles) = content.as_array()
    {
        for article in articles {
            let mut g = convert_article(host, article).await;
            let feed_id = article.get("feed_id").and_then(Value::as_i64).unwrap_or(0) as i32;
            if let Some(&cat) = feed_cat.get(&feed_id) {
                if article
                    .get("feed_title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                {
                    g["origin"]["title"] = json!(escape_to_unicode_alternative(
                        cat_titles
                            .get(&cat)
                            .map(String::as_str)
                            .unwrap_or("Uncategorized"),
                        true
                    ));
                }
                if let Some(name) = cat_titles.get(&cat) {
                    g["categories"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(format!("user/-/label/{name}")));
                }
            }
            items.push(g);
        }
    }
    let key = "published";
    if order == "o" {
        items.sort_by(|a, b| {
            a.get(key)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .cmp(&b.get(key).and_then(Value::as_i64).unwrap_or(0))
        });
    } else {
        items.sort_by(|a, b| {
            b.get(key)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .cmp(&a.get(key).and_then(Value::as_i64).unwrap_or(0))
        });
    }
    // No trailing newline here (the reference's FeedMe endpoint is the
    // exception).
    json_result(
        &json!({
            "id": "user/-/state/com.google/reading-list",
            "updated": chrono::Utc::now().timestamp(),
            "items": items,
        }),
        false,
    )
}

// ── Edits & listings ────────────────────────────────────────────────────────

/// `editTag` (freshapi.php:1195): read/starred via `updateArticle` (one
/// call per id — the reference's loop), labels via direct
/// `ttrss_user_labels2` writes (create-on-miss).
async fn edit_tag(
    host: &HostCtx,
    route: &RouteCtx,
    authed: &Authed,
    e_ids: &[String],
    a: &str,
    r: &str,
) -> MethodOutput {
    let (mode, field): (Option<i64>, Option<i64>) = if a == "user/-/state/com.google/read" {
        (Some(0), Some(2)) // mark read
    } else if r == "user/-/state/com.google/read" {
        (Some(1), Some(2)) // mark unread
    } else if a == "user/-/state/com.google/starred" {
        (Some(1), Some(0)) // star
    } else if r == "user/-/state/com.google/starred" {
        (Some(0), Some(0)) // unstar
    } else {
        (None, None)
    };
    if let (Some(mode), Some(field)) = (mode, field) {
        let is_nnn = host
            .server_env
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
            .map(|(_, v)| v.starts_with("NetNewsWire"))
            .unwrap_or(false);
        for e_id in e_ids {
            let bare = e_id.rsplit('/').next().unwrap_or(e_id);
            let is_dec = !bare.is_empty()
                && bare.bytes().all(|b| b.is_ascii_digit())
                && !bare.starts_with('0');
            let article_id: i64 = if is_dec && !is_nnn {
                bare.parse().unwrap_or(0)
            } else {
                hex2dec(bare)
            };
            let mut ubody = Map::new();
            ubody.insert("article_ids".into(), json!(article_id));
            ubody.insert("mode".into(), json!(mode));
            ubody.insert("field".into(), json!(field));
            let (status, _) = api::update_article(&route.app, &route.sess, &ubody).await;
            let _ = status;
        }
    }

    let add_label: Option<String> = if a.starts_with("user/-/label/") {
        let l = util::clean_str(a.strip_prefix("user/-/label/").unwrap_or(a));
        if l.is_empty() { None } else { Some(l) }
    } else {
        None
    };
    let remove_label: Option<String> = r.strip_prefix("user/-/label/").map(util::clean_str);

    if add_label.is_some() || remove_label.is_some() {
        let pool = host.pool();
        let is_nnn = host
            .server_env
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
            .map(|(_, v)| v.starts_with("NetNewsWire"))
            .unwrap_or(false);
        for e_id in e_ids {
            let bare = e_id.rsplit('/').next().unwrap_or(e_id);
            let is_dec = !bare.is_empty()
                && bare.bytes().all(|b| b.is_ascii_digit())
                && !bare.starts_with('0');
            let article_id: i32 = (if is_dec && !is_nnn {
                bare.parse::<i64>().unwrap_or(0)
            } else {
                hex2dec(bare)
            }) as i32;
            if let Some(l) = &add_label {
                if labels::find_id(pool, l, authed.uid).await == 0 {
                    // `Labels::create($add_label, '', '', $uid)`
                    let db = Db::from_pool(pool.clone());
                    let _ = labels::create(&db, l, None, None, authed.uid, None).await;
                }
                labels::add_article(pool, article_id, l, authed.uid).await;
            }
            if let Some(l) = &remove_label {
                labels::remove_article(pool, article_id, l, authed.uid).await;
            }
        }
    }

    text(200, "OK")
}

/// `unreadCount` (freshapi.php:745): `getCounters` reshaped into GR
/// `unreadcounts` (feed/label/reading-list ids + 16-digit usec timestamps).
async fn unread_count(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
) -> MethodOutput {
    let category_titles: std::collections::HashMap<i32, String> =
        categories(host, route, locale, authed)
            .await
            .into_iter()
            .collect();
    let (lstatus, lcontent) = api::get_labels(&route.app, Some(authed.uid), &Map::new()).await;
    let mut label_titles: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
    if lstatus == api::STATUS_OK
        && let Some(list) = lcontent.as_array()
    {
        for l in list {
            if let (Some(id), Some(caption)) = (
                l.get("id").and_then(Value::as_i64),
                l.get("caption").and_then(Value::as_str),
            ) {
                label_titles.insert(id as i32, caption.to_string());
            }
        }
    }
    let (status, counters) = api::get_counters(&route.app, &route.sess, &Map::new()).await;
    if status != api::STATUS_OK {
        return internal_error();
    }

    let mut unreadcounts: Vec<Value> = Vec::new();
    let mut total_unreads = 0i64;
    if let Some(list) = counters.as_array() {
        for c in list {
            let count = c.get("counter").and_then(Value::as_i64).unwrap_or(0);
            // `str_pad(strval($ts), 16, "0", STR_PAD_RIGHT)` — zero-padded
            // (no-op when already ≥ 16 digits, like PHP).
            let ts = c.get("ts").and_then(Value::as_i64).unwrap_or(0).to_string();
            let last_update = format!("{ts}{}", "0".repeat(16usize.saturating_sub(ts.len())));
            let id: Option<String> = if c.get("kind").and_then(Value::as_str) == Some("cat") {
                let cid = c.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
                if cid == crate::cats::CAT_SPECIAL || cid == crate::cats::CAT_LABELS {
                    continue; // not folders; tag/list doesn't list them either
                }
                let title = category_titles
                    .get(&cid)
                    .cloned()
                    .or_else(|| c.get("title").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                Some(format!("user/-/label/{}", util::html_entity_decode(&title)))
            } else if c.get("title").is_some() {
                Some(format!(
                    "feed/{}",
                    c.get("id").and_then(Value::as_i64).unwrap_or(0)
                ))
            } else if c
                .get("id")
                .and_then(Value::as_i64)
                .map(|v| v != 0)
                .unwrap_or(false)
                && c.get("description").is_some()
            {
                let lid = c.get("id").and_then(Value::as_i64).unwrap_or(0) as i32;
                let title = label_titles
                    .get(&lid)
                    .cloned()
                    .or_else(|| {
                        c.get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                Some(format!("user/-/label/{}", util::html_entity_decode(&title)))
            } else if c.get("id").and_then(Value::as_str) == Some("global-unread") {
                total_unreads = count;
                Some("user/-/state/com.google/reading-list".to_string())
            } else {
                None
            };
            if let Some(id) = id {
                unreadcounts.push(json!({
                    "id": id,
                    "count": count,
                    "newestItemTimestampUsec": last_update,
                }));
            }
        }
    }
    json_result(
        &json!({ "max": total_unreads, "unreadcounts": unreadcounts }),
        true,
    )
}

/// `tagList` (freshapi.php:354): the starred state, the user's folders
/// (`type: folder`), the user's labels (`type: tag`).
async fn tag_list(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
) -> MethodOutput {
    let app: &crate::AppState = &route.app;
    let mut tags: Vec<Value> = vec![json!({ "id": "user/-/state/com.google/starred" })];
    for (id, title) in categories(host, route, locale, authed).await {
        // by id: the titles are translated (the Special/Labels virtuals
        // are not tags)
        if id != crate::cats::CAT_SPECIAL && id != crate::cats::CAT_LABELS {
            tags.push(json!({
                "id": format!("user/-/label/{}", util::html_entity_decode(&title)),
                "type": "folder",
            }));
        }
    }
    let (status, content) = api::get_labels(app, Some(authed.uid), &Map::new()).await;
    if status == api::STATUS_OK
        && let Some(list) = content.as_array()
    {
        for l in list {
            if let Some(caption) = l.get("caption").and_then(Value::as_str) {
                tags.push(json!({
                    "id": format!("user/-/label/{}", util::html_entity_decode(caption)),
                    "type": "tag",
                }));
            }
        }
    }
    json_result(&json!({ "tags": tags }), true)
}

/// `token` (freshapi.php:265): 57 hex chars of `sha256(sid . salt)` —
/// clients send it back as `T=`, the reference never verifies it.
async fn token(host: &HostCtx, authed: &Authed) -> MethodOutput {
    let salt: String = sqlx::query_scalar("SELECT salt FROM ttrss_users WHERE id = $1")
        .bind(authed.uid)
        .fetch_optional(host.pool())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{}{}", authed.sid, salt).as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    text(200, &format!("{}\n", &hex[..57]))
}

/// `userInfo` (freshapi.php:343): the login name everywhere, no email.
fn user_info(sess: &session::Sess) -> MethodOutput {
    let data = sess.data();
    let user = data.name().unwrap_or("");
    json_result(
        &json!({
            "userId": user,
            "userName": user,
            "userProfileId": user,
            "userEmail": "",
        }),
        false,
    )
}

// ── Management (phase 2) ────────────────────────────────────────────────────

/// `requireWriteAccess` (freshapi.php:299): read-only users (access level
/// -1) get a plain 403.
fn forbidden() -> MethodOutput {
    text(403, "Forbidden!")
}

/// `userFeeds` (freshapi.php:330): the owner's feeds (id, title, feed_url,
/// site_url, cat_id with NULL → 0), ordered by title.
async fn user_feeds(host: &HostCtx, authed: &Authed) -> Vec<(i32, String, String, String, i32)> {
    let rows = sqlx::query(
        "SELECT id, title, feed_url, site_url, COALESCE(cat_id, 0) AS cat_id
         FROM ttrss_feeds WHERE owner_uid = $1 ORDER BY title",
    )
    .bind(authed.uid)
    .fetch_all(host.pool())
    .await
    .unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            Some((
                r.try_get::<i32, _>("id").ok()?,
                r.try_get::<String, _>("title").ok()?,
                r.try_get::<String, _>("feed_url").ok()?,
                r.try_get::<String, _>("site_url").ok()?,
                r.try_get::<i32, _>("cat_id").ok()?,
            ))
        })
        .collect()
}

/// `subscriptionList` (freshapi.php:452). The per-feed category title comes
/// from the id→title map; feeds with `cat_id = 0` (uncategorized) get "".
async fn subscription_list(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
) -> MethodOutput {
    let app: &crate::AppState = &route.app;
    let cat_map: std::collections::HashMap<i32, String> = categories(host, route, locale, authed)
        .await
        .into_iter()
        .collect();
    let mut subscriptions = Vec::new();
    for (id, title, feed_url, site_url, cat_id) in user_feeds(host, authed).await {
        let cat_title = cat_map
            .get(&cat_id)
            .map(|t| util::html_entity_decode(t))
            .unwrap_or_default();
        subscriptions.push(json!({
            "id": format!("feed/{id}"),
            "title": title,
            "categories": [{
                "id": format!("user/-/label/{cat_title}"),
                "label": cat_title,
            }],
            "url": feed_url,
            "htmlUrl": site_url,
            // `TTRSS_SELF_URL_PATH . '/public.php?op=feed_icon&id=…'`
            "iconUrl": format!(
                "{}/public.php?op=feed_icon&id={id}.ico",
                app.config.self_url_rtrim()
            ),
        }));
    }
    json_result(&json!({ "subscriptions": subscriptions }), true)
}

/// `subscriptionExport` (freshapi.php:389): `OPML::opml_export` with
/// `include_settings = false`. The reference emits the OPML with PHP's
/// default content type (text/html); the port uses the opml module's
/// declared type.
async fn subscription_export(app: &crate::AppState, sess: &session::Sess) -> MethodOutput {
    let extra = Map::new();
    let (_status, _content_type, _disposition, body) = opml::export(app, sess, &extra).await;
    MethodOutput::Response {
        status: 200,
        // `opml::export`'s declared type (a `&'static str` on the variant)
        content_type: "application/xml+opml",
        body,
        headers: cors(),
    }
}

/// `subscriptionImport` (freshapi.php:400): validate the body is OPML, run
/// the port's `opml::import` directly on the bytes (the reference writes a
/// temp file first — the documented deviation).
async fn subscription_import(
    app: &crate::AppState,
    sess: &session::Sess,
    body: &str,
) -> MethodOutput {
    if !body.to_ascii_lowercase().contains("<opml") {
        return text(400, "Bad Request!");
    }
    let out = opml::import(app, sess, Some(body.as_bytes())).await;
    // DEV: the reference echoes the import log + "Done!", but the
    // write-endpoint convention (FreshRSS, rssmonster) is a bare `OK`;
    // the only GR client that parses the body (greader_api) is broken
    // for every server by its own "Ok:"/"OK:" case bug, so the log
    // goes to the server log instead.
    tracing::info!(body = %collapse_nbsp_br(&out), "OPML import");
    text(200, "OK")
}

/// `preg_replace('/(&nbsp;|<br\/>)+/', "\n", s)` (freshapi.php:429).
fn collapse_nbsp_br(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("&nbsp;") || s[i..].starts_with("<br/>") {
            while i < s.len() && (s[i..].starts_with("&nbsp;") || s[i..].starts_with("<br/>")) {
                i += if s[i..].starts_with("&nbsp;") { 6 } else { 5 };
            }
            out.push('\n');
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// `createCategory` (freshapi.php:556): insert, 0 on failure/empty name.
async fn create_category(host: &HostCtx, uid: i32, name: &str) -> i32 {
    let name = util::clean_str(name);
    if name.is_empty() {
        return 0;
    }
    sqlx::query_scalar(
        "INSERT INTO ttrss_feed_categories (title, owner_uid) VALUES ($1, $2) RETURNING id",
    )
    .bind(&name)
    .bind(uid)
    .fetch_optional(host.pool())
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
}

/// `renameFeed` (freshapi.php:494): `UPDATE … SET title` (success = the
/// statement ran; 0-affected-rows is not a failure, as in PDO).
async fn rename_feed(host: &HostCtx, uid: i32, feed_id: i32, title: &str) -> bool {
    sqlx::query("UPDATE ttrss_feeds SET title = $1 WHERE id = $2 AND owner_uid = $3")
        .bind(util::clean_str(title))
        .bind(feed_id)
        .bind(uid)
        .execute(host.pool())
        .await
        .is_ok()
}

/// `addCategoryFeed` / `removeCategoryFeed` (freshapi.php:576/593): set the
/// feed's folder, `None` = uncategorized (NULL).
async fn set_feed_category(host: &HostCtx, uid: i32, feed_id: i32, cat: Option<i32>) -> bool {
    let res = match cat {
        Some(c) => {
            sqlx::query("UPDATE ttrss_feeds SET cat_id = $1 WHERE id = $2 AND owner_uid = $3")
                .bind(c)
                .bind(feed_id)
                .bind(uid)
                .execute(host.pool())
                .await
        }
        None => {
            sqlx::query("UPDATE ttrss_feeds SET cat_id = NULL WHERE id = $1 AND owner_uid = $2")
                .bind(feed_id)
                .bind(uid)
                .execute(host.pool())
                .await
        }
    };
    res.is_ok()
}

/// `subscribeFeed` (freshapi.php:696): the tt-rss `subscribeToFeed` API call
/// (`content.status` is the `Feeds::_subscribe` result array), then the
/// title lookup. The URL is passed as-is (the reference stopped
/// HTML-escaping it — `&` in query strings broke).
async fn subscribe_feed(
    host: &HostCtx,
    app: &crate::AppState,
    sess: &session::Sess,
    authed: &Authed,
    url: &str,
    category_id: i32,
) -> Result<(i32, String), String> {
    let url = url.strip_prefix("feed/").unwrap_or(url).to_string();
    let mut body = Map::new();
    body.insert("feed_url".into(), json!(url));
    body.insert("category_id".into(), json!(category_id));
    let (status, content) = api::subscribe_to_feed(app, sess, &body).await;
    let rc = content.get("status");
    let feed_id = rc
        .and_then(|v| v.get("feed_id"))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;
    if status != api::STATUS_OK || feed_id <= 0 {
        let code = rc
            .and_then(|v| v.get("code"))
            .and_then(Value::as_i64)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into());
        return Err(format!("Failed to add feed (code {code})"));
    }
    let title =
        sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2")
            .bind(feed_id)
            .bind(authed.uid)
            .fetch_optional(host.pool())
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    Ok((feed_id, title))
}

/// `quickadd` (freshapi.php:726).
async fn quickadd(host: &HostCtx, route: &RouteCtx, authed: &Authed, url: &str) -> MethodOutput {
    let (app, sess) = (&route.app, &route.sess);
    match subscribe_feed(host, app, sess, authed, url, 0).await {
        Ok((feed_id, name)) => json_result(
            &json!({
                "numResults": 1,
                "query": url,
                "streamId": format!("feed/{feed_id}"),
                "streamName": name,
            }),
            false,
        ),
        Err(e) => json_result(&json!({ "numResults": 0, "error": e }), false),
    }
}

/// `subscriptionEdit` (freshapi.php:605): the per-stream `subscribe` /
/// `unsubscribe` / `edit` actions; category from `a` (0 = none, -1 =
/// "Uncategorized" and other virtuals), title from `t`, removal from `r`.
#[allow(clippy::too_many_arguments)] // mirrors the reference signature
async fn subscription_edit(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    stream_names: &[String],
    titles: &[String],
    action: &str,
    add: &str,
    remove: &str,
) -> MethodOutput {
    let (app, sess) = (&route.app, &route.sess);
    let add_name = if let Some(n) = add.strip_prefix("user/-/label/") {
        n.to_string()
    } else {
        String::new()
    };
    let mut category_id: i32 = 0;
    if !add_name.is_empty()
        && let Some(id) = category_label_id(host, route, locale, authed, &add_name).await
    {
        // the virtual categories all mean "no folder"
        category_id = if id > 0 { id } else { -1 };
    }

    for (i, stream_url) in stream_names.iter().enumerate() {
        let Some(rest) = stream_url.strip_prefix("feed/") else {
            continue;
        };
        // doubling up: some readers push a double `feed/` prefix
        let rest = rest.strip_prefix("feed/").unwrap_or(rest);
        let feed_id = if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            rest.parse::<i32>().unwrap_or(0)
        } else {
            feed_id_by_url(host, authed, rest).await
        };
        let title = titles.get(i).cloned().unwrap_or_default();

        match action {
            "subscribe" => {
                if feed_id == 0
                    && subscribe_feed(host, app, sess, authed, rest, category_id.max(0))
                        .await
                        .is_err()
                {
                    return text(400, "Bad Request!");
                }
            }
            "unsubscribe" => {
                if feed_id > 0 {
                    let mut body = Map::new();
                    body.insert("feed_id".into(), json!(feed_id));
                    let (status, _) = api::unsubscribe_feed(app, sess, &body).await;
                    if status != api::STATUS_OK {
                        return text(400, "Bad Request!");
                    }
                }
            }
            "edit" => {
                if feed_id > 0 {
                    // remove first: clients move a feed by sending both
                    // `r=<old>` and `a=<new>`
                    if remove.starts_with("user/-/label/")
                        && !set_feed_category(host, authed.uid, feed_id, None).await
                    {
                        return text(400, "Bad Request!");
                    }
                    if !add_name.is_empty() {
                        if category_id == 0 {
                            // create the folder once, even when several
                            // feeds are moved into it
                            category_id = create_category(host, authed.uid, &add_name).await;
                        }
                        let ok = if category_id > 0 {
                            set_feed_category(host, authed.uid, feed_id, Some(category_id)).await
                        } else {
                            set_feed_category(host, authed.uid, feed_id, None).await
                        };
                        if !ok {
                            return text(400, "Bad Request!");
                        }
                    }
                    if !title.is_empty() && !rename_feed(host, authed.uid, feed_id, &title).await {
                        return text(400, "Bad Request!");
                    }
                } else {
                    return text(400, "Bad Request!");
                }
            }
            _ => {}
        }
    }
    text(200, "OK")
}

/// `renameTag` (freshapi.php:1291): a category wins over a label with the
/// same name.
async fn rename_tag(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    s: &str,
    dest: &str,
) -> MethodOutput {
    let app: &crate::AppState = &route.app;
    if !s.is_empty()
        && s.starts_with("user/-/label/")
        && !dest.is_empty()
        && dest.starts_with("user/-/label/")
    {
        let old_name = &s["user/-/label/".len()..];
        let new_name = util::clean_str(&dest["user/-/label/".len()..]);
        if new_name.is_empty() {
            return text(400, "Bad Request!");
        }
        for (id, title) in categories(host, route, locale, authed).await {
            if name_matches(&title, old_name) {
                let res = sqlx::query(
                    "UPDATE ttrss_feed_categories SET title = $1 WHERE id = $2 AND owner_uid = $3",
                )
                .bind(&new_name)
                .bind(id)
                .bind(authed.uid)
                .execute(host.pool())
                .await;
                if res.is_ok() {
                    return text(200, "OK");
                }
            }
        }
        let (status, content) = api::get_labels(app, Some(authed.uid), &Map::new()).await;
        if status == api::STATUS_OK
            && let Some(list) = content.as_array()
        {
            for l in list {
                if let Some(caption) = l.get("caption").and_then(Value::as_str)
                    && name_matches(caption, old_name)
                {
                    let res = sqlx::query(
                        "UPDATE ttrss_labels2 SET caption = $1 WHERE caption = $2 AND owner_uid = $3",
                    )
                    .bind(&new_name)
                    .bind(caption)
                    .bind(authed.uid)
                    .execute(host.pool())
                    .await;
                    if res.is_ok() {
                        return text(200, "OK");
                    }
                }
            }
        }
    }
    text(400, "Bad Request!")
}

/// `disableTag` (freshapi.php:1338): a category is emptied of feeds and
/// deleted; a label detaches its articles and is deleted.
async fn disable_tag(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    s: &str,
) -> MethodOutput {
    let app: &crate::AppState = &route.app;
    if !s.is_empty() && s.starts_with("user/-/label/") {
        let tag = &s["user/-/label/".len()..];
        for (id, title) in categories(host, route, locale, authed).await {
            if id > 0 && name_matches(&title, tag) {
                let moved = sqlx::query(
                    "UPDATE ttrss_feeds SET cat_id = NULL WHERE cat_id = $1 AND owner_uid = $2",
                )
                .bind(id)
                .bind(authed.uid)
                .execute(host.pool())
                .await;
                if moved.is_err() {
                    return text(400, "Bad Request!");
                }
                // `deleteCategory`: refuse if the folder is not empty
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM ttrss_feeds WHERE cat_id = $1 AND owner_uid = $2",
                )
                .bind(id)
                .bind(authed.uid)
                .fetch_optional(host.pool())
                .await
                .ok()
                .flatten()
                .unwrap_or(0);
                if count != 0 {
                    return text(400, "Bad Request!");
                }
                let res = sqlx::query(
                    "DELETE FROM ttrss_feed_categories WHERE id = $1 AND owner_uid = $2",
                )
                .bind(id)
                .bind(authed.uid)
                .execute(host.pool())
                .await;
                if res.is_ok() {
                    return text(200, "OK");
                }
                return text(400, "Bad Request!");
            }
        }
        let (status, content) = api::get_labels(app, Some(authed.uid), &Map::new()).await;
        if status == api::STATUS_OK
            && let Some(list) = content.as_array()
        {
            for l in list {
                if let Some(caption) = l.get("caption").and_then(Value::as_str)
                    && name_matches(caption, tag)
                {
                    let label_id: Option<i32> = sqlx::query_scalar(
                        "SELECT id FROM ttrss_labels2 WHERE caption = $1 AND owner_uid = $2",
                    )
                    .bind(caption)
                    .bind(authed.uid)
                    .fetch_optional(host.pool())
                    .await
                    .ok()
                    .flatten();
                    let Some(label_id) = label_id else {
                        return text(400, "Bad Request!");
                    };
                    let detached =
                        sqlx::query("DELETE FROM ttrss_user_labels2 WHERE label_id = $1")
                            .bind(label_id)
                            .execute(host.pool())
                            .await;
                    if detached.is_err() {
                        return text(400, "Bad Request!");
                    }
                    let res =
                        sqlx::query("DELETE FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2")
                            .bind(label_id)
                            .bind(authed.uid)
                            .execute(host.pool())
                            .await;
                    if res.is_ok() {
                        return text(200, "OK");
                    }
                    return text(400, "Bad Request!");
                }
            }
        }
    }
    text(400, "Bad Request!")
}

/// `timestampToSeconds` (freshapi.php:1454): clients send micro per spec,
/// some send seconds/millis/nanos.
fn timestamp_to_seconds(ts: &str) -> i64 {
    // PHP `(int)`: saturates on overflow.
    let v = ts.parse::<i64>().unwrap_or(i64::MAX);
    if v >= 100_000_000_000_000_000 {
        v / 1_000_000_000
    } else if v >= 100_000_000_000_000 {
        v / 1_000_000
    } else if v >= 100_000_000_000 {
        v / 1_000
    } else {
        v
    }
}

/// `markAllAsRead` (freshapi.php:1417): every unread article of the stream,
/// bounded by the `ts` cutoff on `date_entered`. Not write-gated in the
/// reference (it is not subscription management).
async fn mark_all_as_read(
    host: &HostCtx,
    route: &RouteCtx,
    locale: &Option<String>,
    authed: &Authed,
    s: &str,
    ts: &str,
) -> MethodOutput {
    let stream_id = normalize_stream_id(s.trim());
    let mut conds: Vec<String> = vec![
        "a.owner_uid = $1".into(),
        "a.unread = true".into(),
        "e.id = a.ref_id".into(),
    ];
    let mut args: Vec<i64> = vec![authed.uid as i64];
    if stream_id == "user/-/state/com.google/starred" {
        conds.push("a.marked = true".into());
    } else if stream_id != "user/-/state/com.google/reading-list" {
        let Some((scope_cond, scope_args)) =
            stream_scope(host, route, locale, authed, &stream_id).await
        else {
            return text(400, "Bad Request!");
        };
        // The scope conditions carry their own `$N` placeholders; renumber
        // past the args bound so far (the reference merges its `?`
        // placeholders positionally).
        let offset = args.len();
        let mut renumbered = String::new();
        let rest = &scope_cond;
        // (straightforward scan: `$<digit>` runs become `$<digit + offset>`)
        let mut i = 0;
        while i < rest.len() {
            let ch = rest[i..].chars().next().unwrap();
            if ch == '$'
                && let Some(d) = rest[i + 1..].chars().next().and_then(|c| c.to_digit(10))
            {
                renumbered.push_str(&format!("${}", d + offset as u32));
                i += 2;
            } else {
                renumbered.push(ch);
                i += ch.len_utf8();
            }
        }
        conds.push(renumbered);
        args.extend(scope_args.iter().map(|a| *a as i64));
    }
    let cutoff = timestamp_to_seconds(ts);
    if cutoff > 0 {
        conds.push(format!(
            "e.date_entered < (to_timestamp(${}) AT TIME ZONE 'UTC')",
            args.len() + 1
        ));
        args.push(cutoff);
    }
    // All fragments are code constants (the scope conditions come from
    // `stream_scope`'s fixed set).
    let where_sql: String = conds.join(" AND ");
    let mut res = sqlx::query(AssertSqlSafe(format!(
        "UPDATE ttrss_user_entries a SET unread = false, last_read = NOW()
         FROM ttrss_entries e WHERE {where_sql}"
    )));
    for a in &args {
        res = res.bind(*a);
    }
    match res.execute(host.pool()).await {
        Ok(_) => text(200, "OK"),
        Err(_) => internal_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_stream_id, urldecode};

    /// PHP `urldecode` is lenient: valid sequences decode, invalid ones
    /// stay literal. Pinned against PHP's behavior.
    #[test]
    fn test_urldecode_lenient() {
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("%41%zz"), "A%zz"); // partial: A decodes, %zz stays
        assert_eq!(urldecode("100%"), "100%"); // trailing %
        assert_eq!(urldecode("%%41"), "%A"); // first %% → literal %, then %41
        assert_eq!(urldecode("%C3%A9"), "\u{e9}"); // multi-byte UTF-8
        assert_eq!(urldecode("%c"), "%c"); // non-hex
        assert_eq!(urldecode(""), "");
    }

    /// `normalizeStreamId` (freshapi.php:898): only a leading
    /// `user/<x>/(state|label)/` is rewritten; `user/` elsewhere (feed
    /// URLs) is untouched.
    #[test]
    fn test_normalize_stream_id() {
        assert_eq!(
            normalize_stream_id("user/1234/state/com.google/starred"),
            "user/-/state/com.google/starred"
        );
        assert_eq!(
            normalize_stream_id("user/joe/label/inbox"),
            "user/-/label/inbox"
        );
        assert_eq!(
            normalize_stream_id("user/-/state/com.google/read"),
            "user/-/state/com.google/read"
        );
        assert_eq!(
            normalize_stream_id("feed/https://example.com/user/joe/rss"),
            "feed/https://example.com/user/joe/rss"
        );
        assert_eq!(normalize_stream_id("feed/3"), "feed/3");
    }
}
