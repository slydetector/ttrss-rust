//! Plugin route registration (docs/22, phase 0): the axum fallback
//! consults `PluginHost::lookup_route` before the static handler, hands
//! the owning plugin the raw request (`handle_route`), and emits the
//! plugin's `MethodOutput` as-is (status, Content-Type, body, extra
//! headers). The router stays a dumb pipe: 405 filtering happens before
//! the plugin is called; everything else is the plugin's.
//!
//! A stub plugin claims `/stub` with `GET, POST, OPTIONS` and echoes
//! what it received, so the tests assert the full request/response
//! round trip over HTTP.

use crate::common;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use std::sync::Arc;
use test_db::template::with_test_db;
use tower::ServiceExt;
use ttrss::AppState;
use ttrss::http;
use ttrss::pluginhost::host::PluginHost;
use ttrss::pluginhost::plugin::{About, MethodOutput, Plugin, RouteRequest};

const CORS: [(&str, &str); 3] = [
    ("access-control-allow-origin", "*"),
    ("access-control-allow-headers", "Authorization"),
    ("access-control-allow-methods", "GET, POST"),
];

fn cors() -> Vec<(String, String)> {
    CORS.map(|(k, v)| (k.to_string(), v.to_string())).to_vec()
}

/// Echoes the raw request: `<method> <path_info> <query> <body>`.
/// `OPTIONS` gets the stub's own 204 (the plugin's to answer, per the
/// phase-0 contract) + the CORS block.
#[derive(Default)]
struct Stub;

#[async_trait]
impl Plugin for Stub {
    fn name(&self) -> &'static str {
        "stub_route"
    }
    fn class_name(&self) -> &'static str {
        "StubRoute"
    }
    fn about(&self) -> About {
        About::new("route test stub", "test")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_route("/stub", &[Method::GET, Method::POST, Method::OPTIONS]);
    }
    async fn handle_route(
        &self,
        _host: &mut ttrss::pluginhost::host::HostCtx,
        req: &RouteRequest<'_>,
    ) -> MethodOutput {
        if req.method == Method::OPTIONS {
            return MethodOutput::Response {
                status: 204,
                content_type: "",
                body: String::new(),
                headers: cors(),
            };
        }
        MethodOutput::Response {
            status: 200,
            content_type: "text/plain; charset=utf-8",
            body: format!(
                "{} {} {} {}",
                req.method.as_str(),
                req.path_info,
                req.query,
                String::from_utf8_lossy(req.body)
            ),
            headers: cors(),
        }
    }
}

async fn make_state(url: &str) -> AppState {
    let db = common::make_db_from_url(url).await.expect("test db");
    let mut state = AppState::new(common::test_config(), db);
    // The stub-only host (the router only needs the route registry).
    state.plugins = Arc::new(PluginHost::startup(&[|| Box::new(Stub)]));
    state
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

async fn text(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

async fn run(state: &AppState) {
    // 1. GET /stub/a/b?x=1 — path_info + query reach the plugin, and its
    //    Response (status, Content-Type, body, headers) comes back verbatim.
    let resp = app(state.clone())
        .oneshot(req(Method::GET, "/tt-rss/stub/a/b?x=1", ""))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/plain; charset=utf-8"
    );
    for (k, v) in CORS {
        assert_eq!(
            resp.headers().get(k).unwrap().to_str().unwrap(),
            v,
            "CORS header {k}"
        );
    }
    assert_eq!(text(resp).await, "GET a/b x=1 ");

    // 2. OPTIONS /stub — the stub's own 204 + CORS headers are delivered.
    let resp = app(state.clone())
        .oneshot(req(Method::OPTIONS, "/tt-rss/stub", ""))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .unwrap()
            .to_str()
            .unwrap(),
        "*",
    );

    // 3. DELETE /stub — 405 from the router, the stub is not called
    //    (a call would have produced 200).
    let resp = app(state.clone())
        .oneshot(req(Method::DELETE, "/tt-rss/stub", ""))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

    // 4. GET /stubby — segment boundary: not the stub's route, falls
    //    through to the static handler (no such file → 404).
    let resp = app(state.clone())
        .oneshot(req(Method::GET, "/tt-rss/stubby", ""))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 5. POST /stub with a body — the raw body reaches the stub
    //    (the OPML-import shape).
    let resp = app(state.clone())
        .oneshot(req(Method::POST, "/tt-rss/stub", "<opml>bytes</opml>"))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(text(resp).await, "POST   <opml>bytes</opml>");
}

#[tokio::test]
async fn plugin_route_round_trip() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            run(&state).await;
            Ok(())
        })
    })
    .await
    .unwrap();
}

/// A duplicate route prefix panics at registration (boot time), per the
/// phase-0 contract (deliberately stricter than `add_api_method`).
#[test]
fn duplicate_route_prefix_panics() {
    #[derive(Default)]
    struct Other;
    #[async_trait]
    impl Plugin for Other {
        fn name(&self) -> &'static str {
            "stub_route_other"
        }
        fn class_name(&self) -> &'static str {
            "StubRouteOther"
        }
        fn about(&self) -> About {
            About::new("second claimant", "test")
        }
        fn api_version(&self) -> i32 {
            2
        }
        fn init(&self, host: &mut PluginHost) {
            host.add_route("/stub", &[Method::GET]);
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PluginHost::startup(&[|| Box::new(Stub), || Box::new(Other)])
    }));
    assert!(result.is_err(), "duplicate prefix must panic");
}
