//! OPML import/export — port of the PHP `OPML` class.
//!
//! This block implements the **export** side (`export` + `opml_export`); the
//! import side lands in a follow-up block. The output is *functionally
//! equivalent* OPML (same feeds/categories, and the settings section when
//! requested) rather than a byte-for-byte match of PHP's `DOMDocument` output.

use std::collections::HashMap;

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::Row;

use crate::session::Sess;
use crate::AppState;

/// The serializable OPML document (quick-xml `serde` types). `Outline` is
/// shared by category and feed nodes; attribute fields are `Option`s so they
/// are omitted when unset (`skip_serializing_if`). `type` is declared before
/// `text` to match the PHP attribute order.
#[derive(serde::Serialize)]
struct Doc {
    #[serde(rename = "@version")]
    version: String,
    head: Head,
    body: Body,
}

#[derive(serde::Serialize)]
struct Head {
    #[serde(rename = "dateCreated")]
    date_created: String,
    #[serde(rename = "title")]
    title: String,
}

#[derive(serde::Serialize)]
struct Body {
    #[serde(rename = "outline")]
    outlines: Vec<Outline>,
}

#[derive(serde::Serialize)]
struct Outline {
    #[serde(rename = "@type", skip_serializing_if = "Option::is_none")]
    r#type: Option<String>,
    #[serde(rename = "@text", skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "@xmlUrl", skip_serializing_if = "Option::is_none")]
    xml_url: Option<String>,
    #[serde(rename = "@ttrssSortOrder", skip_serializing_if = "Option::is_none")]
    ttrss_sort: Option<i64>,
    #[serde(rename = "@ttrssPurgeInterval", skip_serializing_if = "Option::is_none")]
    ttrss_purge: Option<i64>,
    #[serde(rename = "@ttrssUpdateInterval", skip_serializing_if = "Option::is_none")]
    ttrss_update: Option<i64>,
    #[serde(rename = "@htmlUrl", skip_serializing_if = "Option::is_none")]
    html_url: Option<String>,
    #[serde(rename = "outline")]
    children: Vec<Outline>,
}
struct Cat {
    id: i64,
    title: String,
    parent: Option<i64>,
    order_id: i64,
}

struct Feed {
    title: String,
    feed_url: String,
    site_url: String,
    order_id: i64,
    update_interval: i64,
    purge_interval: i64,
    cat_id: Option<i64>,
}

/// A feed outline. `htmlUrl` is always emitted when the site URL is set
/// (independent of `include_settings`); the `ttrss*` attrs are emitted only
/// when `include_settings`.
fn feed_outline(f: &Feed, include_settings: bool) -> Outline {
    Outline {
        r#type: Some("rss".into()),
        text: Some(f.title.clone()),
        xml_url: Some(f.feed_url.clone()),
        ttrss_sort: include_settings.then_some(f.order_id),
        ttrss_purge: include_settings.then_some(f.purge_interval),
        ttrss_update: include_settings.then_some(f.update_interval),
        html_url: (!f.site_url.is_empty()).then(|| f.site_url.clone()),
        children: Vec::new(),
    }
}

/// A category outline (its children are pre-built sub-categories + feeds).
fn cat_outline(c: &Cat, include_settings: bool, children: Vec<Outline>) -> Outline {
    Outline {
        r#type: None,
        text: Some(c.title.clone()),
        xml_url: None,
        ttrss_sort: include_settings.then_some(c.order_id),
        ttrss_purge: None,
        ttrss_update: None,
        html_url: None,
        children,
    }
}

async fn fetch_cats(pool: &sqlx::PgPool, uid: i64) -> Vec<Cat> {
    let rows = sqlx::query(
        "SELECT id, title, parent_cat, order_id FROM ttrss_feed_categories \
         WHERE owner_uid = $1 ORDER BY order_id, title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .expect("fetch categories");
    rows.iter()
        .map(|r| Cat {
            id: r.get::<i32, _>(0) as i64,
            title: r.get(1),
            parent: r.get::<Option<i32>, _>(2).map(|v| v as i64),
            order_id: r.get::<i32, _>(3) as i64,
        })
        .collect()
}

async fn fetch_feeds(pool: &sqlx::PgPool, uid: i64, hide_private: bool) -> Vec<Feed> {
    let q = if hide_private {
        "SELECT title, feed_url, site_url, order_id, update_interval, purge_interval, cat_id \
         FROM ttrss_feeds WHERE owner_uid = $1 AND (private IS false AND auth_login = '' AND auth_pass = '') \
         ORDER BY order_id, title"
    } else {
        "SELECT title, feed_url, site_url, order_id, update_interval, purge_interval, cat_id \
         FROM ttrss_feeds WHERE owner_uid = $1 ORDER BY order_id, title"
    };
    let rows = sqlx::query(q)
        .bind(uid)
        .fetch_all(pool)
        .await
        .expect("fetch feeds");
    rows.iter()
        .map(|r| Feed {
            title: r.get(0),
            feed_url: r.get(1),
            site_url: r.get(2),
            order_id: r.get::<i32, _>(3) as i64,
            update_interval: r.get::<i32, _>(4) as i64,
            purge_interval: r.get::<i32, _>(5) as i64,
            cat_id: r.get::<Option<i32>, _>(6).map(|v| v as i64),
        })
        .collect()
}

/// Assemble the OPML outline tree (root = top-level categories + uncategorized
/// feeds), built iteratively (post-order over the category tree). Within each
/// category, sub-categories precede feeds, matching the PHP
/// `opml_export_category`.
fn build_tree(cats: &[Cat], feeds: &[Feed], include_settings: bool) -> Vec<Outline> {
    let feeds_by_cat: HashMap<Option<i64>, Vec<&Feed>> = feeds
        .iter()
        .fold(HashMap::new(), |mut m, f| {
            m.entry(f.cat_id).or_default().push(f);
            m
        });
    let cat_children: HashMap<Option<i64>, Vec<&Cat>> = cats
        .iter()
        .fold(HashMap::new(), |mut m, c| {
            m.entry(c.parent).or_default().push(c);
            m
        });
    let cat_by_id: HashMap<i64, &Cat> = cats.iter().map(|c| (c.id, c)).collect();

    // Post-order: build each category once its sub-categories are built.
    let mut built: HashMap<i64, Outline> = HashMap::new();
    let mut stack: Vec<(i64, bool)> = cat_children
        .get(&None)
        .map(|v| v.iter().map(|c| (c.id, false)).collect())
        .unwrap_or_default();
    while let Some((id, processed)) = stack.pop() {
        if !processed {
            stack.push((id, true));
            if let Some(children) = cat_children.get(&Some(id)) {
                for c in children {
                    stack.push((c.id, false));
                }
            }
        } else {
            let mut children: Vec<Outline> = Vec::new();
            if let Some(subcats) = cat_children.get(&Some(id)) {
                for sc in subcats {
                    children.push(built.remove(&sc.id).expect("sub-cat built"));
                }
            }
            if let Some(fs) = feeds_by_cat.get(&Some(id)) {
                for f in fs {
                    children.push(feed_outline(f, include_settings));
                }
            }
            let cat = cat_by_id[&id];
            built.insert(id, cat_outline(cat, include_settings, children));
        }
    }

    // Root: top-level categories (in order), then uncategorized feeds.
    let mut root: Vec<Outline> = Vec::new();
    if let Some(tops) = cat_children.get(&None) {
        for t in tops {
            root.push(built.remove(&t.id).expect("top cat built"));
        }
    }
    if let Some(uncat) = feeds_by_cat.get(&None) {
        for f in uncat {
            root.push(feed_outline(f, include_settings));
        }
    }
    root
}

/// Build the OPML document (head + body + feed tree).
pub async fn build_opml(pool: &sqlx::PgPool, uid: i64, include_settings: bool) -> String {
    let cats = fetch_cats(pool, uid).await;
    let feeds = fetch_feeds(pool, uid, false).await;
    let outlines = build_tree(&cats, &feeds, include_settings);

    let date_created = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S %z").to_string();

    // TODO(opml): settings section (tt-rss-prefs/labels/filters) when `include_settings`.
    let doc = Doc {
        version: "1.0".into(),
        head: Head {
            date_created,
            title: "Tiny Tiny RSS Feed Export".into(),
        },
        body: Body { outlines },
    };

    let mut buf = String::new();
    let mut ser = quick_xml::se::Serializer::with_root(&mut buf, Some("opml")).expect("valid root");
    ser.indent(' ', 2);
    doc.serialize(ser).expect("serialize opml");

    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n{buf}\n")
}

/// `OPML::export` — the HTTP handler: builds the OPML for download.
pub async fn export(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> (StatusCode, String, Option<String>, String) {
    let uid = sess.data().uid().unwrap_or(0);
    let include_settings = extra
        .get("include_settings")
        .map(|v| v.as_str() == Some("1") || v.as_bool() == Some(true))
        .unwrap_or(false);

    let name = sess
        .data()
        .get_str("name")
        .unwrap_or("user")
        .replace(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'), "");
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let output_name = format!("tt-rss_{name}_{date}.opml");

    let opml = build_opml(state.db.pool(), uid, include_settings).await;
    (
        StatusCode::OK,
        "application/xml+opml".to_string(),
        Some(format!("attachment; filename={output_name}")),
        opml,
    )
}
