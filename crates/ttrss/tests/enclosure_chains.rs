//! M7.22 item 6 — the `Article::_format_enclosures` chains through the real
//! `Feeds::view` flow:
//!
//! - `HOOK_RENDER_ENCLOSURE` (af_youtube_embed): a YouTube enclosure is
//!   rendered as the reference embedded-player markup (the verbatim
//!   multi-line form with the 4/5/4 tab indentation, NOT the M2 single-line
//!   guess that used to be hardcoded in `format_enclosures`);
//! - `HOOK_ENCLOSURE_ENTRY` (cache_starred_images): an entry whose media has
//!   a DiskCache copy gets its `content_url` rewritten to the local URL.

use crate::common;

use std::path::PathBuf;

use serde_json::Value;
use test_db::template::with_test_db;
use ttrss::AppState;
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::diskcache::DiskCache;
use ttrss::session::Sess;
use ttrss::util::sha1_hex;

fn test_config(plugins: &str, icons_dir: PathBuf) -> Config {
    let mut cfg = common::test_config();
    cfg.plugins = plugins.to_string();
    cfg.cache_dir = icons_dir.parent().map(PathBuf::from).unwrap_or_default();
    cfg
}

/// The view flow's plugin cache roots at `config.cache_dir` (the parent of the
/// icons dir) — so the test's DiskCache files must live under that root exactly.
fn cache_dirs() -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("ttrss-enc-chains-{}", std::process::id()));
    let icons = base.join("icons");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&icons).unwrap();
    (base, icons)
}

fn sess() -> Sess {
    let mut d = Session::new();
    d.set("uid", PValue::Int(1));
    d.set(
        "access_level",
        PValue::Int(ttrss::core::user::ACCESS_LEVEL_ADMIN as i64),
    );
    d.set(
        "pwd_hash",
        PValue::Bytes(b"SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8".to_vec()),
    );
    d.set("auth_module", PValue::Bytes(b"auth_internal".to_vec()));
    d.set("name", PValue::Bytes(b"admin".to_vec()));
    d.set("csrf_token", PValue::Bytes(b"tok".to_vec()));
    let s = Sess::new("enc-chains".into());
    s.replace("enc-chains".into(), d);
    s
}

async fn view_feed(state: &AppState, feed: &str) -> Value {
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri("/backend.php")
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(axum::body::Body::from(format!(
            "op=Feeds&method=view&feed={feed}&csrf_token=tok"
        )))
        .unwrap();
    let res = ttrss::rpc::handler(
        axum::extract::State(state.clone()),
        axum::extract::Extension(sess()),
        req,
    )
    .await;
    let bytes = axum::body::to_bytes(res.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

// HOOK_RENDER_ENCLOSURE — the YouTube enclosure is rendered by
// af_youtube_embed with the reference's verbatim markup (tabs included —
// the pre-fix hardcoded copy was a single-line double-quote guess).
#[tokio::test]
async fn test_view_render_enclosure_chain_youtube() {
    with_test_db(|url| Box::pin(async move {
    let (base, icons) = cache_dirs();
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let state = AppState::new(test_config("af_youtube_embed", icons), db);
    // A YouTube watch-URL enclosure on article 2 (feed 1).
    sqlx::query(
        "INSERT INTO ttrss_enclosures (content_url, content_type, title, duration, width, height, post_id) \
         VALUES ('https://www.youtube.com/watch?v=dQw4w9WgXcQ', 'video', 'yt', '0', 0, 0, 2)",
    )
    .execute(state.db.pool())
    .await
    .unwrap();

    let v = view_feed(&state, "1").await;
    let formatted = v
        .pointer("/headlines/content/1/enclosures/formatted")
        .and_then(|f| f.as_str())
        .unwrap_or("");
    assert!(
        formatted.contains("youtube-nocookie.com/embed/dQw4w9WgXcQ"),
        "formatted: {formatted}"
    );
    // The reference's exact shape: newlines + tab indentation (4 before
    // `<iframe`, 5 before the attributes, 4 before `</div>`), single quotes.
    assert!(
        formatted.contains("\n\t\t\t\t<iframe class='youtube-player'"),
        "reference tab indentation missing: {formatted:?}"
    );
    assert!(
        formatted
            .contains("\n\t\t\t\t\ttype='text/html' width='640' height='385'"),
        "reference attribute lines missing: {formatted:?}"
    );
    assert!(
        formatted.contains("frameborder='0'></iframe>\n\t\t\t\t</div>"),
        "reference closing shape missing: {formatted:?}"
    );
    // The enclosure did NOT also land in `entries` (PHP: rendered enclosures
    // are skipped from the entry list).
    let entries = v
        .pointer("/headlines/content/1/enclosures/entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(entries.is_empty(), "entries: {entries:?}");
    let _ = std::fs::remove_dir_all(&base);
        Ok(())
    })).await.unwrap();
}

// HOOK_ENCLOSURE_ENTRY — cache_starred_images rewrites `content_url` to the
// local DiskCache URL when a cached copy exists (the pre-fix entry went
// through no chain at all).
#[tokio::test]
async fn test_view_enclosure_entry_chain_local_cache() {
    with_test_db(|url| {
        Box::pin(async move {
            let (base, icons) = cache_dirs();
            let db: Db = common::make_db_from_url(&url).await.expect("test db");
            let state = AppState::new(test_config("cache_starred_images", icons), db);
            {
                // Article 1's seeded enclosure (img.youtube.com/.../hqdefault.jpg)
                // gets a cached copy the way the plugin's task would: the article
                // id + the sha1 of the content_url.
                let cache =
                    DiskCache::new(&base, "starred-images", "http://127.0.0.1:9000/tt-rss/");
                let url = "https://img.youtube.com/vi/dQw4w9WgXcQ/hqdefault.jpg";
                let name = format!("1-{}", sha1_hex(url));
                cache.put(&name, b"fake-bytes");
                assert!(cache.exists(&name), "cache file seeded");
            }

            let v = view_feed(&state, "1").await;
            let entries = v
                .pointer("/headlines/content/0/enclosures/entries")
                .and_then(|e| e.as_array())
                .cloned()
                .unwrap_or_default();
            let urls: Vec<&str> = entries
                .iter()
                .filter_map(|e| e.get("content_url").and_then(Value::as_str))
                .collect();
            assert_eq!(urls.len(), 2, "article 1 has 2 seeded enclosures: {urls:?}");
            assert!(
                urls.iter().any(|u| u.contains("starred-images/1-")),
                "expected a local cache URL in the entry list: {urls:?}"
            );
            // The other entry (the mp4, uncached) keeps its remote URL.
            assert!(
                urls.contains(&"https://example.com/video.mp4"),
                "uncached entry must stay remote: {urls:?}"
            );
            let _ = std::fs::remove_dir_all(&base);
            Ok(())
        })
    })
    .await
    .unwrap();
}
