//! Response compression (tower-http `CompressionLayer`: gzip + brotli + zstd).
//!
//! Parity target: the official image's `web-nginx` container (nginx `gzip on`,
//! `gzip_comp_level 6`). That is nginx-side behavior — there is no PHP test to
//! port, so the bar here is Rust-only: per-request `Accept-Encoding`
//! negotiation (including q-values), decodable bodies, `Vary` correctness, and
//! the `image/*` exclusion. Legacy clients get gzip-6 (the official parity
//! case); modern browsers get zstd/br (a strict superset of the official).

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use std::io::Read as _;
use test_db::template::with_test_db;
use tower::ServiceExt;
use ttrss::AppState;
use ttrss::http;

/// The frozen reference tree (the docroot for static-asset tests).
fn tt_rss_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss")
}

async fn make_state(url: &str) -> AppState {
    let mut cfg = common::test_config();
    cfg.docroot = tt_rss_dir();
    let db = common::make_db_from_url(url).await.expect("test db");
    AppState::new(cfg, db)
}

fn app(state: AppState) -> Router {
    http::build_router(&state).with_state(state)
}

/// GET with an optional `Accept-Encoding`; returns the response.
async fn get(app: Router, uri: &str, accept_encoding: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(ae) = accept_encoding {
        builder = builder.header(header::ACCEPT_ENCODING, ae);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("request succeeds")
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body")
        .to_vec()
}

/// GET the (unauthenticated) login page — a realistic text/html body.
async fn login_page(app: Router, accept_encoding: Option<&str>) -> axum::response::Response {
    get(app, "/tt-rss/", accept_encoding).await
}

fn content_encoding(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[tokio::test]
async fn gzip_negotiated_decodes() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), Some("gzip")).await;
            assert_eq!(content_encoding(&resp).as_deref(), Some("gzip"));
            // nginx parity: Vary so caches don't serve the encoded body to
            // clients that did not advertise gzip.
            let vary = resp.headers().get_all(header::VARY).iter().any(|v| {
                v.to_str()
                    .unwrap_or("")
                    .split(',')
                    .any(|p| p.trim().eq_ignore_ascii_case("accept-encoding"))
            });
            assert!(vary, "Vary: Accept-Encoding must be present");
            let decoded = {
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(&body_bytes(resp).await[..])
                    .read_to_end(&mut out)
                    .expect("gzip body decodes");
                out
            };
            let html = String::from_utf8(decoded).expect("valid utf-8");
            assert!(html.len() > 1000 && html.to_ascii_lowercase().contains("<html"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn brotli_negotiated_decodes() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), Some("br")).await;
            assert_eq!(content_encoding(&resp).as_deref(), Some("br"));
            let raw = body_bytes(resp).await;
            let mut out = Vec::new();
            brotli::Decompressor::new(&raw[..], 4096)
                .read_to_end(&mut out)
                .expect("brotli body decodes");
            let html = String::from_utf8(out).expect("valid utf-8");
            assert!(html.len() > 1000 && html.to_ascii_lowercase().contains("<html"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn zstd_negotiated_decodes() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), Some("zstd")).await;
            assert_eq!(content_encoding(&resp).as_deref(), Some("zstd"));
            let raw = body_bytes(resp).await;
            let out = zstd::decode_all(&raw[..]).expect("zstd body decodes");
            let html = String::from_utf8(out).expect("valid utf-8");
            assert!(html.len() > 1000 && html.to_ascii_lowercase().contains("<html"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn deflate_not_offered() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            // deflate is legacy; the official image doesn't offer it either —
            // a client advertising only deflate gets the identity body.
            let resp = login_page(app(state), Some("deflate")).await;
            assert_eq!(content_encoding(&resp), None);
            let raw = body_bytes(resp).await;
            let html = String::from_utf8(raw).expect("valid utf-8");
            assert!(html.to_ascii_lowercase().contains("<html"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn no_accept_encoding_identity() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), None).await;
            assert_eq!(content_encoding(&resp), None);
            let html = String::from_utf8(body_bytes(resp).await).expect("valid utf-8");
            assert!(html.to_ascii_lowercase().contains("<html"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn identity_negotiated() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), Some("identity")).await;
            assert_eq!(content_encoding(&resp), None);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn multi_value_picks_an_offered_encoding() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            let resp = login_page(app(state), Some("gzip, br")).await;
            let enc = content_encoding(&resp).expect("a response encoding is set");
            assert!(
                matches!(enc.as_str(), "gzip" | "br"),
                "expected gzip or br, got {enc}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn q_value_zero_means_not_acceptable() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            // gzip is advertised with q=0 (not acceptable); zstd must win.
            let resp = login_page(app(state), Some("gzip;q=0, zstd")).await;
            assert_eq!(content_encoding(&resp).as_deref(), Some("zstd"));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn image_asset_not_compressed() {
    with_test_db(|url| {
        Box::pin(async move {
            let state = make_state(&url).await;
            // The default predicate excludes image/* (nginx's gzip_types omits it too).
            let resp = get(app(state), "/tt-rss/images/blank_icon.gif", Some("gzip")).await;
            assert_eq!(resp.status(), axum::http::StatusCode::OK);
            assert_eq!(content_encoding(&resp), None);
            let mime = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                mime.starts_with("image/"),
                "expected an image mime, got {mime}"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
}
