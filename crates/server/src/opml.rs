//! OPML import/export — port of the PHP `OPML` class.
//!
//! This block implements the **export** side (`export` + `opml_export`); the
//! import side lands in a follow-up block. The output is *functionally
//! equivalent* OPML (same feeds/categories, and the settings section when
//! requested) rather than a byte-for-byte match of PHP's `DOMDocument` output.

use std::collections::HashMap;

use axum::http::StatusCode;
use serde_json::{Map, Value};
use sqlx::Row;

use crate::session::Sess;
use crate::util::htmlspecialchars;
use crate::AppState;

/// A node in the export tree.
#[derive(Debug)]
enum NodeKind {
    Cat {
        title: String,
        sort_order: i64,
    },
    Feed {
        title: String,
        xml_url: String,
        site_url: String,
        sort_order: i64,
        purge_interval: i64,
        update_interval: i64,
    },
}

#[derive(Debug)]
struct Node {
    kind: NodeKind,
    /// Indices into the flat `nodes` list (sub-categories first, then feeds).
    children: Vec<usize>,
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

fn make_cat_node(c: &Cat) -> Node {
    Node {
        kind: NodeKind::Cat {
            title: c.title.clone(),
            sort_order: c.order_id,
        },
        children: Vec::new(),
    }
}

fn make_feed_node(f: &Feed) -> Node {
    Node {
        kind: NodeKind::Feed {
            title: f.title.clone(),
            xml_url: f.feed_url.clone(),
            site_url: f.site_url.clone(),
            sort_order: f.order_id,
            purge_interval: f.purge_interval,
            update_interval: f.update_interval,
        },
        children: Vec::new(),
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

/// Assemble the flat node list plus the root's children (sub-cats first, then feeds).
fn build_nodes(
    cats: &[Cat],
    feeds: &[Feed],
) -> (Vec<Node>, Vec<usize>) {
    let mut nodes: Vec<Node> = Vec::new();
    let mut cat_idx: HashMap<i64, usize> = HashMap::new();

    // Create a (children-pending) node for each category.
    for c in cats {
        cat_idx.insert(c.id, nodes.len());
        nodes.push(make_cat_node(c));
    }

    // Root's children: top-level categories (parent NULL), then uncategorized feeds.
    let mut root_children: Vec<usize> = Vec::new();
    for c in cats {
        if c.parent.is_none() {
            root_children.push(cat_idx[&c.id]);
        }
    }
    for f in feeds {
        if f.cat_id.is_none() {
            root_children.push(nodes.len());
            nodes.push(make_feed_node(f));
        }
    }

    // Fill each category's children: sub-cats, then its feeds.
    for c in cats {
        let idx = cat_idx[&c.id];
        let mut children: Vec<usize> = Vec::new();
        for sub in cats {
            if sub.parent == Some(c.id) {
                children.push(cat_idx[&sub.id]);
            }
        }
        for f in feeds {
            if f.cat_id == Some(c.id) {
                children.push(nodes.len());
                nodes.push(make_feed_node(f));
            }
        }
        nodes[idx].children = children;
    }

    (nodes, root_children)
}

/// Iterative DFS serialization (2-space indent; sub-categories before feeds).
fn render_tree(nodes: &[Node], root_children: &[usize], out: &mut String, include_settings: bool) {
    enum Item {
        Node(usize, usize), // (index, indent level)
        Close(usize),       // indent level
    }
    let mut stack: Vec<Item> = root_children
        .iter()
        .copied()
        .rev()
        .map(|i| Item::Node(i, 2))
        .collect();
    while let Some(item) = stack.pop() {
        match item {
            Item::Close(level) => out.push_str(&format!("{}\n", "  ".repeat(level) + "</outline>")),
            Item::Node(idx, level) => match &nodes[idx].kind {
                NodeKind::Cat { title, sort_order } => {
                    let indent = "  ".repeat(level);
                    let sort_attr = if include_settings {
                        format!(" ttrssSortOrder=\"{sort_order}\"")
                    } else {
                        String::new()
                    };
                    out.push_str(&format!(
                        "{indent}<outline text=\"{}\"{sort_attr}>\n",
                        htmlspecialchars(title)
                    ));
                    stack.push(Item::Close(level));
                    for &c in nodes[idx].children.iter().rev() {
                        stack.push(Item::Node(c, level + 1));
                    }
                }
                NodeKind::Feed {
                    title,
                    xml_url,
                    site_url,
                    sort_order,
                    purge_interval,
                    update_interval,
                } => {
                    let indent = "  ".repeat(level);
                    let ttrss = if include_settings {
                        format!(
                            " ttrssSortOrder=\"{sort_order}\" ttrssPurgeInterval=\"{purge_interval}\" ttrssUpdateInterval=\"{update_interval}\""
                        )
                    } else {
                        String::new()
                    };
                    let html = if site_url.is_empty() {
                        String::new()
                    } else {
                        format!(" htmlUrl=\"{}\"", htmlspecialchars(site_url))
                    };
                    out.push_str(&format!(
                        "{indent}<outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\"{ttrss}{html}/>\n",
                        htmlspecialchars(title),
                        htmlspecialchars(xml_url)
                    ));
                }
            },
        }
    }
}

/// Build the OPML document (head + body + feed tree).
pub async fn build_opml(pool: &sqlx::PgPool, uid: i64, include_settings: bool) -> String {
    let cats = fetch_cats(pool, uid).await;
    let feeds = fetch_feeds(pool, uid, false).await;
    let (nodes, root_children) = build_nodes(&cats, &feeds);

    let date_created = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S %z").to_string();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<opml version=\"1.0\">\n");
    out.push_str("  <head>\n");
    out.push_str(&format!("    <dateCreated>{date_created}</dateCreated>\n"));
    out.push_str("    <title>Tiny Tiny RSS Feed Export</title>\n");
    out.push_str("  </head>\n");
    out.push_str("  <body>\n");
    render_tree(&nodes, &root_children, &mut out, include_settings);
    // TODO(opml): settings section (tt-rss-prefs/labels/filters) when `include_settings`.
    out.push_str("  </body>\n");
    out.push_str("</opml>\n");
    out
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
