//! The freshapi plugin, phase 1 (docs/22): the Google Reader / FreshRSS
//! API surface over HTTP (`/freshapi`), end to end against the fixtures
//! (feeds 1-4, categories 1-2, labels 1-2, entries 1-11, owner 1 =
//! `admin`/`password`).
//!
//! The reference (freshapi/api/freshapi.php) ships no test suite; the
//! cases here pin the v1 endpoint contract: ClientLogin + session auth,
//! stream/contents (all four scopes), stream/items/ids,
//! stream/items/contents, edit-tag, unread-count, tag/list, token,
//! user-info.

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
