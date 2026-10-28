//! The freshapi plugin, phase 1 (docs/22): the Google Reader / FreshRSS
//! API surface over HTTP (`/freshapi`), end to end against the fixtures
//! (feeds 1-4, categories 1-2, labels 1-2, entries 1-11, owner 1 =
//! `admin`/`password`).
//!
//! The reference (freshapi/api/freshapi.php) ships no test suite; the
//! cases here pin the endpoint contract: ClientLogin + session auth,
//! stream/contents (all four scopes), stream/items/ids,
//! stream/items/contents, edit-tag, unread-count, tag/list, token,
//! user-info, and the phase-2 management verbs (subscription
//! list/export/import/edit/quickadd, rename-tag, disable-tag,
//! mark-all-as-read).

use crate::common;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use test_db::template::with_test_db;
use tower::ServiceExt;
use ttrss::AppState;
use ttrss::http;
use ttrss::pluginhost::host::PluginHost;

const BASE: &str = "/tt-rss";

/// The full registry (freshapi must be loadable; the prefs-page list
/// agrees with the host).
fn all_plugin_factories() -> Vec<fn() -> Box<dyn ttrss::pluginhost::Plugin>> {
    ttrss::pluginhost::all_plugins()
}

async fn setup(url: &str) -> (AppState, String) {
    let db = common::make_db_from_url(url).await.expect("test db");
    let mut state = AppState::new(common::test_config(), db);
    state.plugins = Arc::new(PluginHost::startup(&all_plugin_factories()));
    enable_freshapi(&state).await;
    let login = client_login(&state, "admin", "password").await;
    (state, login.sid)
}

fn app(state: AppState) -> Router {
    http::build_router(&state).with_state(state)
}

fn req(method: Method, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn auth_req(method: Method, uri: &str, body: &str, sid: &str) -> Request<Body> {
    let login = "admin";
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("GoogleLogin_auth/{login}/{sid}"))
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn text(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = text(resp).await;
    serde_json::from_str(&body).expect("json body")
}

/// `POST /freshapi/accounts/ClientLogin` → the `SID=admin/<sid>` token.
struct Login {
    status: StatusCode,
    body: String,
    sid: String,
}

async fn client_login(state: &AppState, email: &str, passwd: &str) -> Login {
    let resp = app(state.clone())
        .oneshot(req(
            Method::POST,
            &format!("{BASE}/freshapi/accounts/ClientLogin"),
            &format!("Email={email}&Passwd={passwd}"),
        ))
        .await
        .expect("response");
    let status = resp.status();
    let body = text(resp).await;
    let sid = body
        .lines()
        .find_map(|l| l.strip_prefix("SID="))
        .map(|v| v.split('/').next_back().unwrap_or("").to_string())
        .unwrap_or_default();
    Login { status, body, sid }
}

/// Enable the per-user API gate + freshapi for user 1.
async fn enable_freshapi(state: &AppState) {
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
    )
    .execute(state.db.pool())
    .await
    .expect("set ENABLE_API_ACCESS");
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
         VALUES (1, '_ENABLED_PLUGINS', NULL, 'freshapi')",
    )
    .execute(state.db.pool())
    .await
    .expect("enable freshapi");
}

#[tokio::test]
async fn test_client_login() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let mut state = AppState::new(common::test_config(), db);
            state.plugins = Arc::new(PluginHost::startup(&all_plugin_factories()));
            enable_freshapi(&state).await;

            let resp = app(state.clone())
                .oneshot(req(
                    Method::POST,
                    &format!("{BASE}/freshapi/accounts/ClientLogin"),
                    "Email=admin&Passwd=password",
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers()
                    .get("content-type")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "text/plain; charset=UTF-8",
            );
            assert_eq!(
                resp.headers()
                    .get("cache-control")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "no-store",
            );
            assert_eq!(
                resp.headers()
                    .get("access-control-allow-origin")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "*",
            );
            let body = text(resp).await;
            let sid = body
                .lines()
                .find_map(|l| l.strip_prefix("SID="))
                .map(|v| v.split('/').next_back().unwrap_or("").to_string())
                .unwrap_or_default();
            assert!(sid.len() > 10, "sid: {body:?}");
            assert_eq!(body, format!("SID=admin/{sid}\nLSID=\nAuth=admin/{sid}\n"),);

            // A bad password: 401 + Google-Bad-Token.
            let resp = app(state.clone())
                .oneshot(req(
                    Method::POST,
                    &format!("{BASE}/freshapi/accounts/ClientLogin"),
                    "Email=admin&Passwd=wrong",
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                resp.headers()
                    .get("google-bad-token")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "true",
            );
            assert_eq!(text(resp).await, "Unauthorized!");
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_freshapi_disabled_for_user() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");
            let mut state = AppState::new(common::test_config(), db);
            state.plugins = Arc::new(PluginHost::startup(&all_plugin_factories()));
            // API access on, but no `_ENABLED_PLUGINS` row: freshapi is off.
            sqlx::query(
                "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value)
                 VALUES (1, 'ENABLE_API_ACCESS', NULL, 'true')",
            )
            .execute(state.db.pool())
            .await
            .expect("set ENABLE_API_ACCESS");
            let login = client_login(&state, "admin", "password").await;
            assert_eq!(login.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                login.body,
                "API access or the FreshAPI plugin is not enabled for this user in the TT-RSS Preferences!",
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_auth_gates() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;

            // No Authorization header → 401.
            let resp = app(state.clone())
                .oneshot(req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/user-info"),
                    "",
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

            // Wrong user in the token → 401 (the session is admin's).
            let resp = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(format!("{BASE}/freshapi/reader/api/0/user-info"))
                        .header("Authorization", format!("GoogleLogin_auth/nobody/{sid}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

            // Bogus session id → 401 (no such session row).
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/user-info"),
                    "",
                    "no-such-sid",
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

            // Healthcheck + bad paths.
            let resp = app(state.clone())
                .oneshot(req(Method::GET, &format!("{BASE}/freshapi"), ""))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(text(resp).await, "OK");

            // The freshapi-compat mount answers identically (the drop-in
            // URL a PHP tt-rss + freshapi client is configured with).
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!(
                        "{BASE}/plugins.local/freshapi/api/greader.php/reader/api/0/user-info"
                    ),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            assert_eq!(v["userId"], "admin");

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/stream"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            assert_eq!(text(resp).await, "Bad Request!");

            // OPTIONS → 204 + CORS.
            let resp = app(state.clone())
                .oneshot(req(Method::OPTIONS, &format!("{BASE}/freshapi"), ""))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
            assert_eq!(
                resp.headers()
                    .get("access-control-allow-headers")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "Authorization",
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_user_info_and_token() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/user-info"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers()
                    .get("content-type")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "application/json; charset=UTF-8",
            );
            let v = json_body(resp).await;
            assert_eq!(v["userId"], "admin");
            assert_eq!(v["userName"], "admin");
            assert_eq!(v["userProfileId"], "admin");
            assert_eq!(v["userEmail"], "");

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/token"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = text(resp).await;
            let token = body.strip_suffix('\n').unwrap();
            assert_eq!(token.len(), 57, "token: {body:?}");
            assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_stream_contents_scopes() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let get = |path: &str| {
                let (state, sid) = (&state, sid.clone());
                let path = path.to_string();
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::GET,
                            &format!("{BASE}/freshapi/reader/api/0/{path}"),
                            "",
                            &sid,
                        ))
                        .await
                        .expect("response");
                    assert_eq!(resp.status(), StatusCode::OK, "{path}");
                    json_body(resp).await
                })
            };

            // reading-list: the 8 unread fixture articles, hex ids, the
            // GR shape (origin/summary/categories), continuation at n<8.
            let v = get("stream/contents/user/-/state/com.google/reading-list?n=3").await;
            assert_eq!(v["id"], "user/-/state/com.google/reading-list");
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 3);
            assert_eq!(v["continuation"], "3");
            let first = &items[0];
            let id = first["id"].as_str().unwrap();
            assert!(id.starts_with("tag:google.com,2005:reader/item/"), "{id}");
            let hex = &id["tag:google.com,2005:reader/item/".len()..];
            // `dec2hex`: 16 zero-padded hex chars.
            assert_eq!(hex.len(), 16, "{id}");
            let n: i64 = i64::from_str_radix(hex, 16).unwrap();
            assert!((1..=11).contains(&n), "fixture id: {n}");
            assert_eq!(first["crawlTimeMsec"], format!("{}000", first["published"]));
            assert_eq!(
                first["timestampUsec"],
                format!("{}000000", first["published"])
            );
            assert!(
                first["categories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c == "user/-/state/com.google/reading-list")
            );
            assert!(
                first["origin"]["streamId"]
                    .as_str()
                    .unwrap()
                    .starts_with("feed/")
            );
            assert!(first["summary"]["content"].as_str().unwrap().contains('<'));
            assert!(first["origin"]["htmlUrl"].as_str().is_some());

            // starred: view_mode marked → the 3 marked fixture articles.
            let v = get("stream/contents/user/-/state/com.google/starred").await;
            assert_eq!(v["id"], "user/-/state/com.google/starred");
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 3);
            for it in items {
                assert!(
                    it["categories"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c == "user/-/state/com.google/starred")
                );
            }

            // feed/1: the 2 unread HN articles (3 is read).
            let v = get("stream/contents/feed/1").await;
            assert_eq!(v["id"], "feed/1");
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 2);
            for it in items {
                assert_eq!(it["origin"]["streamId"], "feed/1");
            }
            // feed/1 by URL.
            let v = get("stream/contents/feed/https%3A%2F%2Fhnrss.org%2Ffrontpage").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 2);

            // unknown numeric feed id: an empty stream, no continuation
            // (`count` is left at 20 → `totalFetched == count` is false).
            let v = get("stream/contents/feed/999").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 0);
            assert!(v.get("continuation").is_none());

            // unknown feed URL: `count = 0` → `totalFetched == count` → an
            // empty stream with a continuation.
            let v = get("stream/contents/feed/no-such-feed.example").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 0);
            assert_eq!(v["continuation"].as_str(), Some("0"));

            // category: Technology → feeds 1-3 → 6 unread.
            let v = get("stream/contents/user/-/label/Technology").await;
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 6);

            // label (empty in the fixtures): 0 items, not an error.
            let v = get("stream/contents/user/-/label/test-label-1").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 0);
            assert!(v.get("continuation").is_none());

            // article 1 carries its two enclosures: the reference emits
            // the raw duration when `intval()` != 0 — "05:30" stays the
            // string "05:30" (not 5); the image's 0 → no length key.
            let v = get("stream/contents/feed/1?n=10").await;
            let items = v["items"].as_array().unwrap();
            let with_enc = items
                .iter()
                .find(|it| it["id"].as_str().map(|i| i.ends_with("1")).unwrap_or(false));
            let enc = with_enc.unwrap()["enclosure"].as_array().unwrap();
            assert_eq!(enc.len(), 2);
            assert_eq!(enc[0]["type"], "video/mp4");
            assert_eq!(enc[0]["length"], "05:30");
            assert_eq!(enc[1]["type"], "image/jpeg");
            assert!(enc[1].get("length").is_none());
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_stream_contents_ot() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let get = |path: &str| {
                let (state, sid) = (&state, sid.clone());
                let path = path.to_string();
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::GET,
                            &format!("{BASE}/freshapi/reader/api/0/{path}"),
                            "",
                            &sid,
                        ))
                        .await
                        .expect("response");
                    assert_eq!(resp.status(), StatusCode::OK, "{path}");
                    json_body(resp).await
                })
            };

            // `ot` is the greader protocol's delta knob: "the timestamp of
            // the oldest item that you're interested in". The filter is on
            // `date_entered` (crawl time), so backdated items fetched after
            // `ot` are still returned.
            // Fixture date_entered: 1: 05-25 10:00, 2: 05-25 09:30,
            // 3: 05-24 18:00, 4: 05-25 08:00, 5: 05-24 14:00,
            // 6: 05-24 11:00, 7: 05-25 07:00, 8: 05-24 16:00,
            // 9: 05-25 06:00, 10: 05-24 12:00, 11: 05-23 15:00 (all 2026 UTC).

            // Baseline: all 8 unread.
            let v = get("stream/contents/user/-/state/com.google/reading-list").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 8);

            // ot = 2026-05-24 18:00 UTC (1779645600): eligible entries
            // 1, 2, 3, 4, 7, 9 (>= boundary, article 3 included); unread
            // of those: 1, 2, 4, 7, 9.
            let v = get("stream/contents/user/-/state/com.google/reading-list?ot=1779645600").await;
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 5, "{v}");
            let ids: Vec<i64> = items
                .iter()
                .map(|it| {
                    let hex = it["id"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("tag:google.com,2005:reader/item/")
                        .unwrap();
                    i64::from_str_radix(hex, 16).unwrap()
                })
                .collect();
            assert_eq!(ids.len(), 5);
            for n in [1, 2, 4, 7, 9] {
                assert!(ids.contains(&n), "{n} missing: {ids:?}");
            }

            // ot beyond every fixture: empty stream, no continuation.
            let v = get("stream/contents/user/-/state/com.google/reading-list?ot=1779761600").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 0);
            assert!(v.get("continuation").is_none());

            // Same knob on the starred scope (view_mode marked): starred
            // 3 (05-24 18:00, boundary), 8 (05-24 16:00), 11 (05-23) →
            // only 3 survives the filter.
            let v = get("stream/contents/user/-/state/com.google/starred?ot=1779645600").await;
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 1, "{v}");
            assert!(items[0]["id"].as_str().unwrap().ends_with("03"));

            // ot=0 (absent-equivalent): unfiltered.
            let v = get("stream/contents/user/-/state/com.google/reading-list?ot=0").await;
            assert_eq!(v["items"].as_array().unwrap().len(), 8);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_stream_items_ids() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;

            // reading-list: 8 ids (decimal strings), n=3 → continuation.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/stream/items/ids?s=user/-/state/com.google/reading-list&n=3"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            let refs = v["itemRefs"].as_array().unwrap();
            assert_eq!(refs.len(), 3);
            for r in refs {
                let id = r["id"].as_str().unwrap();
                assert!(id.bytes().all(|b| b.is_ascii_digit()), "{id}");
                let n: i64 = id.parse().unwrap();
                assert!((1..=11).contains(&n));
            }
            let cont = v["continuation"].as_str().unwrap();
            assert!(cont.bytes().all(|b| b.is_ascii_digit()));

            // starred scope.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/stream/items/ids?s=user/-/state/com.google/starred"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            let v = json_body(resp).await;
            let refs: Vec<i64> = v["itemRefs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_eq!(refs, vec![11, 8, 3]); // ref_id DESC

            // feed/2 scope.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/stream/items/ids?s=feed/2"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            let v = json_body(resp).await;
            assert_eq!(v["itemRefs"].as_array().unwrap().len(), 3);

            // read state: "read since ot" keys on `last_read` (NULL in the
            // fixtures) → no ids.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/stream/items/ids?s=user/-/state/com.google/read"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            let v = json_body(resp).await;
            assert!(v["itemRefs"].as_array().unwrap().is_empty());
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_stream_items_contents() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;

            // FeedMe: explicit ids in the POST body (the reference's
            // `multiplePosts` reads `php://input`), the feed's category
            // added.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/stream/items/contents"),
                    "i=tag%3Agoogle.com%2C2005%3Areader%2Fitem%2F1&i=2",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            assert_eq!(v["id"], "user/-/state/com.google/reading-list");
            let items = v["items"].as_array().unwrap();
            assert_eq!(items.len(), 2);
            for it in items {
                assert!(
                    it["categories"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c == "user/-/label/Technology")
                );
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_edit_tag() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let post = |body: &str| {
                let (state, sid) = (&state, sid.clone());
                let body = body.to_string();
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::POST,
                            &format!("{BASE}/freshapi/reader/api/0/edit-tag"),
                            &body,
                            &sid,
                        ))
                        .await
                        .expect("response");
                    (resp.status(), text(resp).await)
                })
            };

            // Mark article 1 read (hex id).
            let (status, body) =
                post("i=tag%3Agoogle.com%2C2005%3Areader%2Fitem%2F1&a=user/-/state/com.google/read").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, "OK");
            let unread: Option<bool> = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE owner_uid = 1 AND ref_id = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(unread, Some(false));

            // Mark it unread again.
            let (status, _) = post("i=1&r=user/-/state/com.google/read").await;
            assert_eq!(status, StatusCode::OK);
            let unread: Option<bool> = sqlx::query_scalar(
                "SELECT unread FROM ttrss_user_entries WHERE owner_uid = 1 AND ref_id = 1",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(unread, Some(true));

            // Star / unstar.
            let (status, _) = post("i=2&a=user/-/state/com.google/starred").await;
            assert_eq!(status, StatusCode::OK);
            let marked: Option<bool> = sqlx::query_scalar(
                "SELECT marked FROM ttrss_user_entries WHERE owner_uid = 1 AND ref_id = 2",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(marked, Some(true));
            let (status, _) = post("i=2&r=user/-/state/com.google/starred").await;
            assert_eq!(status, StatusCode::OK);
            let marked: Option<bool> = sqlx::query_scalar(
                "SELECT marked FROM ttrss_user_entries WHERE owner_uid = 1 AND ref_id = 2",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(marked, Some(false));

            // Add a label (created on miss) + remove it.
            let (status, _) = post("i=4&a=user/-/label/gr-test").await;
            assert_eq!(status, StatusCode::OK);
            let label_rows: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_user_labels2 WHERE article_id = 4",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(label_rows, 1);
            let caption: Option<String> = sqlx::query_scalar(
                "SELECT l.caption FROM ttrss_labels2 l WHERE l.id = (SELECT label_id FROM ttrss_user_labels2 WHERE article_id = 4 LIMIT 1)",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(caption.as_deref(), Some("gr-test"));

            let (status, _) = post("i=4&r=user/-/label/gr-test").await;
            assert_eq!(status, StatusCode::OK);
            let label_rows: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM ttrss_user_labels2 WHERE article_id = 4",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(label_rows, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn test_unread_count_and_tag_list() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/unread-count?output=json"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            assert_eq!(v["max"], 8); // 8 unread fixture articles
            let ids: Vec<&str> = v["unreadcounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect();
            assert!(ids.contains(&"user/-/state/com.google/reading-list"));
            assert!(ids.contains(&"feed/1"));
            assert!(ids.contains(&"user/-/label/Technology"));
            assert!(ids.contains(&"user/-/label/Science & Nature"));
            assert!(ids.contains(&"user/-/label/Uncategorized"));
            assert!(!ids.contains(&"user/-/label/Special"));
            assert!(!ids.contains(&"user/-/label/Labels"));
            // 16-digit usec timestamps.
            for c in v["unreadcounts"].as_array().unwrap() {
                let ts = c["newestItemTimestampUsec"].as_str().unwrap();
                assert_eq!(ts.len(), 16, "{ts}");
            }
            // `output` gate.
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/unread-count"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/tag/list?output=json"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            let tags = v["tags"].as_array().unwrap();
            let starred = tags
                .iter()
                .find(|t| t["id"] == "user/-/state/com.google/starred");
            assert!(starred.is_some());
            let folders: Vec<&str> = tags
                .iter()
                .filter(|t| t["type"] == "folder")
                .map(|t| t["id"].as_str().unwrap())
                .collect();
            assert!(folders.contains(&"user/-/label/Technology"));
            assert!(folders.contains(&"user/-/label/Science & Nature"));
            assert!(folders.contains(&"user/-/label/Uncategorized"));
            let label_ids: Vec<&str> = tags
                .iter()
                .filter(|t| t["type"] == "tag")
                .map(|t| t["id"].as_str().unwrap())
                .collect();
            assert!(label_ids.contains(&"user/-/label/test-label-1"));
            assert!(label_ids.contains(&"user/-/label/test-label-2"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

// ── Phase 2: management verbs ───────────────────────────────────────────────

const MOCK_FEED: &str = include_str!("../../../tt-rss/tests_integration/feed.xml");

/// A canned mock feed server (one good RSS body, repeated).
fn mock_feed_server() -> common::MockServer {
    common::MockServer::new(vec![common::Canned {
        status: 200,
        reason: "OK",
        headers: vec![],
        body: MOCK_FEED.as_bytes().to_vec(),
    }])
}

/// `subscription/list?output=json`: the four fixture feeds in title order,
/// per-feed category label (cat 1/2), the icon URL, url/htmlUrl.
#[tokio::test]
async fn test_subscription_list() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/list?output=json"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let v = json_body(resp).await;
            let subs = v["subscriptions"].as_array().unwrap();
            assert_eq!(subs.len(), 4);
            // ORDER BY title
            let titles: Vec<&str> = subs.iter().map(|s| s["title"].as_str().unwrap()).collect();
            assert_eq!(
                titles,
                vec![
                    "Ars Technica",
                    "Hacker News",
                    "MIT Tech Review",
                    "The Register"
                ]
            );
            let hn = &subs[1];
            assert_eq!(hn["id"], "feed/1");
            assert_eq!(hn["categories"][0]["id"], "user/-/label/Technology");
            assert_eq!(hn["categories"][0]["label"], "Technology");
            assert_eq!(hn["url"], "https://hnrss.org/frontpage");
            assert_eq!(hn["htmlUrl"], "https://news.ycombinator.com");
            assert!(
                hn["iconUrl"]
                    .as_str()
                    .unwrap()
                    .ends_with("/public.php?op=feed_icon&id=1.ico")
            );
            // `output` must be json
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/list"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `subscription/edit`: folder moves (`a=`/`r=`), folder creation for a
/// new `a=`, rename (`t=`), and the double `feed/` prefix.
#[tokio::test]
async fn test_subscription_edit() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let post = |body: &str| {
                let (state, sid) = (&state, sid.clone());
                let body = body.to_string();
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::POST,
                            &format!("{BASE}/freshapi/reader/api/0/subscription/edit"),
                            &body,
                            &sid,
                        ))
                        .await
                        .expect("response");
                    assert_eq!(resp.status(), StatusCode::OK, "{body}");
                    text(resp).await
                })
            };
            let cat_of = |feed: i32| {
                let state = &state;
                Box::pin(async move {
                    sqlx::query_scalar("SELECT cat_id FROM ttrss_feeds WHERE id = $1")
                        .bind(feed)
                        .fetch_optional(state.db.pool())
                        .await
                        .unwrap()
                        .flatten()
                })
            };

            // move feed 4: Science & Nature -> Technology (r + a)
            assert_eq!(
                post("s=feed/4&r=user/-/label/Science%20%26%20Nature&a=user/-/label/Technology&ac=edit").await,
                "OK"
            );
            assert_eq!(cat_of(4).await, Some(1));

            // move feed 4 into a brand-new folder (a= only)
            assert_eq!(
                post("s=feed/4&a=user/-/label/News&ac=edit").await,
                "OK"
            );
            let news_cat = sqlx::query_scalar("SELECT id FROM ttrss_feed_categories WHERE title = 'News' AND owner_uid = 1")
                .fetch_optional(state.db.pool())
                .await
                .unwrap()
                .flatten()
                .expect("News folder created");
            assert_eq!(cat_of(4).await, Some(news_cat));

            // move feed 4 back to uncategorized (r= only)
            assert_eq!(
                post("s=feed/4&r=user/-/label/News&ac=edit").await,
                "OK"
            );
            assert_eq!(cat_of(4).await, None);

            // rename feed 1 (t=)
            assert_eq!(
                post("s=feed/1&t=HN&ac=edit").await,
                "OK"
            );
            let title: String = sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE id = 1")
                .fetch_one(state.db.pool())
                .await
                .unwrap();
            assert_eq!(title, "HN");

            // double `feed/` prefix is stripped
            assert_eq!(
                post("s=feed/feed/1&t=Hacker%20News&ac=edit").await,
                "OK"
            );
            let title: String = sqlx::query_scalar("SELECT title FROM ttrss_feeds WHERE id = 1")
                .fetch_one(state.db.pool())
                .await
                .unwrap();
            assert_eq!(title, "Hacker News");

            // unknown (non-numeric, unresolvable) feed for `edit` -> 400
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/edit"),
                    "s=feed/not-a-real-feed-url&ac=edit",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `subscription/edit` subscribe + unsubscribe against the mock server,
/// and `subscription/quickadd` (success + an invalid URL).
#[tokio::test]
async fn test_subscription_subscribe_unsubscribe_quickadd() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let mock = mock_feed_server();
            let feed_url = mock.url("/feed.xml");

            // subscribe (s=feed/<url>, the reference's wire form)
            let body = format!("s=feed/{feed_url}&ac=subscribe");
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/edit"),
                    &body,
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(text(resp).await, "OK");
            let new_id: i32 = sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE feed_url = $1")
                .bind(&feed_url)
                .fetch_one(state.db.pool())
                .await
                .unwrap();

            // quickadd (success)
            let body = format!("quickadd={}", mock.url("/feed2.xml"));
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/quickadd"),
                    &body,
                    &sid,
                ))
                .await
                .expect("response");
            let v: Value = json_body(resp).await;
            assert_eq!(v["numResults"], 1);
            assert_eq!(
                v["streamId"],
                format!("feed/{}", {
                    let id: i32 =
                        sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE feed_url = $1")
                            .bind(mock.url("/feed2.xml"))
                            .fetch_one(state.db.pool())
                            .await
                            .unwrap();
                    id
                })
            );
            assert!(!v["streamName"].as_str().unwrap().is_empty());

            // quickadd (unfetchable URL -> numResults 0 + error)
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/quickadd"),
                    "quickadd=not-a-url",
                    &sid,
                ))
                .await
                .expect("response");
            let v = json_body(resp).await;
            assert_eq!(v["numResults"], 0);
            assert!(v["error"].as_str().unwrap().contains("Failed to add feed"));

            // unsubscribe
            let body = format!("s=feed/{new_id}&ac=unsubscribe");
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/edit"),
                    &body,
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let gone: i64 = sqlx::query_scalar("SELECT count(*) FROM ttrss_feeds WHERE id = $1")
                .bind(new_id)
                .fetch_one(state.db.pool())
                .await
                .unwrap();
            assert_eq!(gone, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `subscription/export`: the OPML of the fixture feeds; `subscription/import`:
/// an OPML body round-trips new feeds, GET is a 400, non-OPML bodies 400.
#[tokio::test]
async fn test_subscription_export_import() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let mock = mock_feed_server();

            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/export"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers().get("content-type").unwrap(),
                "application/xml+opml"
            );
            let opml = text(resp).await;
            assert!(opml.contains("<opml"));
            assert!(opml.contains("Hacker News"));
            assert!(opml.contains("https://hnrss.org/frontpage"));

            // import: a new feed into a new category
            let body = format!(
                r#"<?xml version="1.0"?>
                <opml version="2.0">
                  <head><title>import</title></head>
                  <body>
                    <outline text="News">
                      <outline text="Imported" type="subscription"
                               xmlUrl="{u}" htmlUrl="https://example.com"/>
                    </outline>
                  </body>
                </opml>"#,
                u = mock.url("/feed3.xml")
            );
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/import"),
                    &body,
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let out = text(resp).await;
            assert!(out.ends_with("Done!"), "{out}");
            let imported: Option<i32> =
                sqlx::query_scalar("SELECT id FROM ttrss_feeds WHERE feed_url = $1")
                    .bind(mock.url("/feed3.xml"))
                    .fetch_optional(state.db.pool())
                    .await
                    .unwrap()
                    .flatten();
            assert!(imported.is_some());

            // GET import -> 400; non-OPML body -> 400
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::GET,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/import"),
                    "",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            let resp = app(state.clone())
                .oneshot(auth_req(
                    Method::POST,
                    &format!("{BASE}/freshapi/reader/api/0/subscription/import"),
                    "hello",
                    &sid,
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `rename-tag` (category wins over a same-named label) and `disable-tag`
/// (a category is emptied of feeds and deleted; a label detaches articles).
#[tokio::test]
async fn test_rename_disable_tag() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let post = |path: &str, body: &str| {
                let (state, sid) = (&state, sid.clone());
                let (path, body) = (path.to_string(), body.to_string());
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::POST,
                            &format!("{BASE}/freshapi/reader/api/0/{path}"),
                            &body,
                            &sid,
                        ))
                        .await
                        .expect("response");
                    let status = resp.status();
                    let body = text(resp).await;
                    (status, body)
                })
            };

            // rename a category
            let (status, body) = post(
                "rename-tag",
                "s=user/-/label/Technology&dest=user/-/label/Tech",
            )
            .await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let title: Option<String> =
                sqlx::query_scalar("SELECT title FROM ttrss_feed_categories WHERE id = 1")
                    .fetch_optional(state.db.pool())
                    .await
                    .unwrap()
                    .flatten();
            assert_eq!(title.as_deref(), Some("Tech"));

            // rename a label
            let (status, body) = post(
                "rename-tag",
                "s=user/-/label/test-label-1&dest=user/-/label/l1-renamed",
            )
            .await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let caption: String =
                sqlx::query_scalar("SELECT caption FROM ttrss_labels2 WHERE id = 1")
                    .fetch_one(state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(caption, "l1-renamed");

            // unknown tag -> 400
            let (status, _) = post("rename-tag", "s=user/-/label/nope&dest=user/-/label/x").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            // disable the category: its 3 feeds go to uncategorized, the
            // row is deleted
            let (status, body) = post("disable-tag", "s=user/-/label/Tech").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let cat_rows: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ttrss_feed_categories WHERE id = 1")
                    .fetch_one(state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(cat_rows, 0);
            let uncats: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ttrss_feeds WHERE cat_id IS NULL")
                    .fetch_one(state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(uncats, 3); // feeds 1-3 moved (feed 4 stays in cat 2)

            // disable a label: article attachments + the row go
            let (status, body) = post("disable-tag", "s=user/-/label/l1-renamed").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let label_rows: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ttrss_labels2 WHERE id = 1")
                    .fetch_one(state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(label_rows, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// `mark-all-as-read`: feed / starred / reading-list scopes, the `ts`
/// cutoff, label scope, unknown stream -> 400.
#[tokio::test]
async fn test_mark_all_as_read() {
    with_test_db(|url| {
        Box::pin(async move {
            let (state, sid) = setup(&url).await;
            let unread = |feed: Option<i32>| {
                let state = &state;
                Box::pin(async move {
                    if let Some(f) = feed {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM ttrss_user_entries WHERE unread AND feed_id = $1",
                        )
                        .bind(f)
                        .fetch_one(state.db.pool())
                        .await
                        .unwrap()
                    } else {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM ttrss_user_entries WHERE unread",
                        )
                        .fetch_one(state.db.pool())
                        .await
                        .unwrap()
                    }
                })
            };

            let post = |body: &str| {
                let (state, sid) = (&state, sid.clone());
                let body = body.to_string();
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            Method::POST,
                            &format!("{BASE}/freshapi/reader/api/0/mark-all-as-read"),
                            &body,
                            &sid,
                        ))
                        .await
                        .expect("response");
                    let status = resp.status();
                    let body = text(resp).await;
                    (status, body)
                })
            };

            // feed 1: its 2 unread articles go read
            assert_eq!(unread(Some(1)).await, 2);
            let (status, body) = post("s=feed/1").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            assert_eq!(unread(Some(1)).await, 0);
            let total = unread(None).await;
            assert!(total > 0, "other feeds stay unread: {total}");

            // reading-list: everything remaining goes read
            let (status, body) = post("s=user/-/state/com.google/reading-list").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            assert_eq!(unread(None).await, 0);

            // starred: mark the read state untouched (all read now) —
            // restore one unread, then mark the starred stream (article 3
            // is the only starred fixture article)
            sqlx::query("UPDATE ttrss_user_entries SET unread = true WHERE ref_id = 3")
                .execute(state.db.pool())
                .await
                .unwrap();
            let (status, body) = post("s=user/-/state/com.google/starred").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let starred_unread: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM ttrss_user_entries WHERE unread AND marked",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(starred_unread, 0);
            let remaining: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ttrss_user_entries WHERE unread")
                    .fetch_one(state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(remaining, 0); // the restored article 3 was starred -> read

            // ts cutoff: restore feed 2 (3 unread, older fixture dates),
            // a cutoff before all of them marks none; after all, marks all
            sqlx::query("UPDATE ttrss_user_entries SET unread = true WHERE feed_id = 2")
                .execute(state.db.pool())
                .await
                .unwrap();
            let (status, body) = post("s=feed/2&ts=1").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            assert_eq!(unread(Some(2)).await, 3);
            let (status, body) = post("s=feed/2&ts=4102444800000000").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            assert_eq!(unread(Some(2)).await, 0);

            // label scope: attach a label to an unread article, mark it
            sqlx::query("UPDATE ttrss_user_entries SET unread = true WHERE ref_id = 5")
                .execute(state.db.pool())
                .await
                .unwrap();
            sqlx::query("INSERT INTO ttrss_user_labels2 (label_id, article_id) VALUES ($1, 5)")
                .bind(2)
                .execute(state.db.pool())
                .await
                .unwrap();
            let (status, body) = post("s=user/-/label/test-label-2").await;
            assert_eq!((status, body), (StatusCode::OK, "OK".to_string()));
            let five_unread: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM ttrss_user_entries WHERE unread AND ref_id = 5",
            )
            .fetch_one(state.db.pool())
            .await
            .unwrap();
            assert_eq!(five_unread, 0);

            // unknown stream -> 400; non-numeric ts -> 400
            let (status, _) = post("s=user/-/label/no-such-label").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let (status, _) = post("s=feed/1&ts=abc").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// The write verbs (import / edit / quickadd / rename / disable) are 403 for
/// a read-only user; the read verbs and mark-all-as-read (not write-gated in
/// the reference) still work.
#[tokio::test]
async fn test_readonly_gates() {
    with_test_db(|url| {
        Box::pin(async move {
            // Read-only from the first login: a level change after the
            // fact would invalidate the session (access level is part of
            // the per-request validation) -> 401, not 403.
            let db = common::make_db_from_url(&url).await.expect("test db");
            let mut state = AppState::new(common::test_config(), db);
            state.plugins = Arc::new(PluginHost::startup(&all_plugin_factories()));
            enable_freshapi(&state).await;
            sqlx::query("UPDATE ttrss_users SET access_level = -1 WHERE id = 1")
                .execute(state.db.pool())
                .await
                .unwrap();
            let sid = client_login(&state, "admin", "password").await.sid;
            let call = |method: Method, path: &str, body: &str| {
                let (state, sid) = (&state, sid.clone());
                let (path, body) = (path.to_string(), body.to_string());
                Box::pin(async move {
                    let resp = app(state.clone())
                        .oneshot(auth_req(
                            method,
                            &format!("{BASE}/freshapi/reader/api/0/{path}"),
                            &body,
                            &sid,
                        ))
                        .await
                        .expect("response");
                    let status = resp.status();
                    let body = text(resp).await;
                    (status, body)
                })
            };
            // the 403s
            assert_eq!(
                call(Method::POST, "subscription/import", "<opml></opml>").await,
                (StatusCode::FORBIDDEN, "Forbidden!".to_string())
            );
            assert_eq!(
                call(Method::POST, "subscription/edit", "s=feed/1&ac=edit&t=x").await,
                (StatusCode::FORBIDDEN, "Forbidden!".to_string())
            );
            assert_eq!(
                call(
                    Method::POST,
                    "subscription/quickadd",
                    "quickadd=http://127.0.0.1:1/f"
                )
                .await,
                (StatusCode::FORBIDDEN, "Forbidden!".to_string())
            );
            assert_eq!(
                call(
                    Method::POST,
                    "rename-tag",
                    "s=user/-/label/a&dest=user/-/label/b"
                )
                .await,
                (StatusCode::FORBIDDEN, "Forbidden!".to_string())
            );
            assert_eq!(
                call(Method::POST, "disable-tag", "s=user/-/label/a").await,
                (StatusCode::FORBIDDEN, "Forbidden!".to_string())
            );
            // reads + mark-all-as-read are not gated
            assert_eq!(
                call(Method::GET, "subscription/list?output=json", "")
                    .await
                    .0,
                StatusCode::OK
            );
            assert_eq!(
                call(Method::GET, "subscription/export", "").await.0,
                StatusCode::OK
            );
            assert_eq!(
                call(Method::POST, "mark-all-as-read", "s=feed/1").await.0,
                StatusCode::OK
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
