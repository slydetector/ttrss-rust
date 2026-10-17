//! OPML import/export — port of the PHP `OPML` class.
//!
//! **Export** (`export` + `opml_export`): builds a typed feed tree and serializes it
//! with `quick-xml` (2-space indent, matching the PHP `DOMDocument` pretty-printing);
//! with `include_settings` it also appends the `tt-rss-prefs` / `tt-rss-labels` /
//! `tt-rss-filters` settings blocks, mirroring the PHP export (the filter payload is a
//! CDATA section of native-typed JSON, so it round-trips through both imports).
//! **Import** (`import` + `opml_import`): parses an uploaded OPML file (quick-xml pull
//! parser) and recreates the user's feed categories + feeds (deduped by `feed_url`),
//! plus the `tt-rss-prefs` / `tt-rss-labels` / `tt-rss-filters` settings sections.
//! The output is *functionally equivalent* OPML rather than a byte-for-byte match
//! of PHP's `DOMDocument` output.

use std::collections::HashMap;

use crate::core::config::SCHEMA_VERSION;
use crate::core::db::Db;
use crate::feeds::{category_title, feed_title};
use crate::i18n::{__, t_sprintf};

use axum::http::StatusCode;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::Row;

use crate::AppState;
use crate::session::Sess;
use crate::util::{format_error, htmlspecialchars, php_int_cast};

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
    ttrss_sort: Option<i32>,
    #[serde(
        rename = "@ttrssPurgeInterval",
        skip_serializing_if = "Option::is_none"
    )]
    ttrss_purge: Option<i32>,
    #[serde(
        rename = "@ttrssUpdateInterval",
        skip_serializing_if = "Option::is_none"
    )]
    ttrss_update: Option<i32>,
    #[serde(rename = "@htmlUrl", skip_serializing_if = "Option::is_none")]
    html_url: Option<String>,
    #[serde(rename = "outline")]
    children: Vec<Outline>,
}
struct Cat {
    id: i32,
    title: String,
    parent: Option<i32>,
    order_id: i32,
}

struct Feed {
    title: String,
    feed_url: String,
    site_url: String,
    order_id: i32,
    update_interval: i32,
    purge_interval: i32,
    cat_id: Option<i32>,
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

async fn fetch_cats(pool: &sqlx::PgPool, uid: i32) -> Vec<Cat> {
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
            id: r.get::<i32, _>(0),
            title: r.get(1),
            parent: r.get::<Option<i32>, _>(2),
            order_id: r.get::<i32, _>(3),
        })
        .collect()
}

async fn fetch_feeds(pool: &sqlx::PgPool, uid: i32, hide_private: bool) -> Vec<Feed> {
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
            order_id: r.get::<i32, _>(3),
            update_interval: r.get::<i32, _>(4),
            purge_interval: r.get::<i32, _>(5),
            cat_id: r.get::<Option<i32>, _>(6),
        })
        .collect()
}

/// Assemble the OPML outline tree (root = top-level categories + uncategorized
/// feeds), built iteratively (post-order over the category tree). Within each
/// category, sub-categories precede feeds, matching the PHP
/// `opml_export_category`.
fn build_tree(cats: &[Cat], feeds: &[Feed], include_settings: bool) -> Vec<Outline> {
    let feeds_by_cat: HashMap<Option<i32>, Vec<&Feed>> =
        feeds.iter().fold(HashMap::new(), |mut m, f| {
            m.entry(f.cat_id).or_default().push(f);
            m
        });
    let cat_children: HashMap<Option<i32>, Vec<&Cat>> =
        cats.iter().fold(HashMap::new(), |mut m, c| {
            m.entry(c.parent).or_default().push(c);
            m
        });
    let cat_by_id: HashMap<i32, &Cat> = cats.iter().map(|c| (c.id, c)).collect();

    // Post-order: build each category once its sub-categories are built.
    let mut built: HashMap<i32, Outline> = HashMap::new();
    let mut stack: Vec<(i32, bool)> = cat_children
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
pub async fn build_opml(pool: &sqlx::PgPool, uid: i32, include_settings: bool) -> String {
    let cats = fetch_cats(pool, uid).await;
    let feeds = fetch_feeds(pool, uid, false).await;
    let outlines = build_tree(&cats, &feeds, include_settings);

    let date_created = chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S %z")
        .to_string();

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

    let mut out = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n{buf}\n");
    if include_settings {
        // The settings blocks become the children of `<body>`. A non-empty body ends in
        // a 2-space-indented `</body>`; the blocks use 4/6 spaces, so inserting them
        // right before `  </body>` keeps them aligned with the feed-tree outlines. An
        // empty body serializes as a self-closing `<body/>`, which is expanded to hold
        // the blocks (a feed-less export can still carry labels/prefs/filters).
        let settings = build_settings_section(pool, uid).await;
        if let Some(idx) = out.rfind("  </body>") {
            let (before, after) = out.split_at(idx);
            out = format!("{before}{settings}{after}");
        } else {
            // Empty body: expand the self-closing `<body/>` to hold the blocks.
            out = out.replace("<body/>", &format!("<body>\n{settings}  </body>\n"));
        }
    }
    out
}

/// Build the three settings `<outline>` blocks (`tt-rss-prefs` / `tt-rss-labels` /
/// `tt-rss-filters`) as a raw XML string, indented to sit as the last children of
/// `<body>` (blocks at 4 spaces, leaves at 6). A raw string (not the serde `Outline`
/// model) so the filter payload can be a real CDATA section: the JSON may contain
/// `&`/`<`/`>`, which must NOT be entity-escaped (the import reads CDATA verbatim,
/// and `Event::Text` is not entity-decoded). `OPML.php:118-222`.
async fn build_settings_section(pool: &sqlx::PgPool, uid: i32) -> String {
    let mut out = String::new();

    // tt-rss-prefs — the `profile IS NULL` user prefs, by name.
    out.push_str(&format!(
        "    <outline text=\"tt-rss-prefs\" schema-version=\"{SCHEMA_VERSION}\">\n"
    ));
    let prefs = sqlx::query(
        "SELECT pref_name, value FROM ttrss_user_prefs2 \
         WHERE profile IS NULL AND owner_uid = $1 ORDER BY pref_name",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .expect("fetch prefs");
    for r in &prefs {
        out.push_str(&format!(
            "      <outline pref-name=\"{}\" value=\"{}\"/>\n",
            htmlspecialchars(&r.get::<String, _>(0)),
            htmlspecialchars(&r.get::<String, _>(1))
        ));
    }
    out.push_str("    </outline>\n");

    // tt-rss-labels — every label with its colors.
    out.push_str(&format!(
        "    <outline text=\"tt-rss-labels\" schema-version=\"{SCHEMA_VERSION}\">\n"
    ));
    let labels =
        sqlx::query("SELECT caption, fg_color, bg_color FROM ttrss_labels2 WHERE owner_uid = $1")
            .bind(uid)
            .fetch_all(pool)
            .await
            .expect("fetch labels");
    for r in &labels {
        out.push_str(&format!(
            "      <outline label-name=\"{}\" label-fg-color=\"{}\" label-bg-color=\"{}\"/>\n",
            htmlspecialchars(&r.get::<String, _>(0)),
            htmlspecialchars(&r.get::<String, _>(1)),
            htmlspecialchars(&r.get::<String, _>(2))
        ));
    }
    out.push_str("    </outline>\n");

    // tt-rss-filters — one CDATA outline per filter (a JSON snapshot).
    out.push_str(&format!(
        "    <outline text=\"tt-rss-filters\" schema-version=\"{SCHEMA_VERSION}\">\n"
    ));
    let filters = sqlx::query("SELECT id FROM ttrss_filters2 WHERE owner_uid = $1 ORDER BY id")
        .bind(uid)
        .fetch_all(pool)
        .await
        .expect("fetch filters");
    for r in &filters {
        let filter_id: i32 = r.get::<i32, _>(0);
        out.push_str(&format!(
            "      <outline filter-type=\"2\"><![CDATA[{}]]></outline>\n",
            build_filter_json(pool, filter_id, uid).await
        ));
    }
    out.push_str("    </outline>\n");

    out
}

/// The JSON snapshot of one filter: its columns (minus `id`/`owner_uid`) plus its
/// `rules` (feed matches rendered as portable titles) and `actions`, in the same key
/// order as the PHP export (`OPML.php:182-215`). Native JSON types (bools/integers)
/// so the payload round-trips losslessly.
async fn build_filter_json(pool: &sqlx::PgPool, filter_id: i32, uid: i32) -> String {
    let f = sqlx::query(
        "SELECT match_any_rule, inverse, title, order_id, last_triggered, enabled \
         FROM ttrss_filters2 WHERE id = $1 AND owner_uid = $2",
    )
    .bind(filter_id)
    .bind(uid)
    .fetch_one(pool)
    .await
    .expect("filter row");

    let mut obj = Map::new();
    obj.insert("match_any_rule".into(), Value::Bool(f.get::<bool, _>(0)));
    obj.insert("inverse".into(), Value::Bool(f.get::<bool, _>(1)));
    obj.insert("title".into(), Value::String(f.get::<String, _>(2)));
    obj.insert("order_id".into(), Value::from(f.get::<i32, _>(3)));
    match f.get::<Option<chrono::NaiveDateTime>, _>(4) {
        Some(ts) => obj.insert(
            "last_triggered".into(),
            Value::String(ts.format("%Y-%m-%d %H:%M:%S").to_string()),
        ),
        None => obj.insert("last_triggered".into(), Value::Null),
    };
    obj.insert("enabled".into(), Value::Bool(f.get::<bool, _>(5)));

    // --- rules ---
    let mut rules: Vec<Value> = Vec::new();
    let rrows = sqlx::query(
        "SELECT reg_exp, inverse, filter_type, feed_id, cat_id, match_on, cat_filter \
         FROM ttrss_filters2_rules WHERE filter_id = $1",
    )
    .bind(filter_id)
    .fetch_all(pool)
    .await
    .expect("filter rules");
    for r in &rrows {
        let reg_exp = r.get::<String, _>(0);
        let inverse: bool = r.get(1);
        let filter_type: i32 = r.get(2);
        let feed_id: Option<i32> = r.get(3);
        let cat_id: Option<i32> = r.get(4);
        let match_on: Option<String> = r.get(5);
        let cat_filter: bool = r.get(6);

        let mut rule = Map::new();
        rule.insert("reg_exp".into(), Value::String(reg_exp));
        rule.insert("inverse".into(), Value::Bool(inverse));
        rule.insert("filter_type".into(), Value::from(filter_type));
        rule.insert("cat_filter".into(), Value::Bool(cat_filter));

        let (is_match, match_on_str) = match &match_on {
            Some(m) if !m.trim().is_empty() => (true, m.clone()),
            _ => (false, String::new()),
        };
        if is_match {
            // `match` triples `[name, is_cat, is_id]`; a missing id's placeholder is the
            // STRING "0" (not the number 0) so the PHP and Rust imports agree.
            let items: Vec<String> = serde_json::from_str(&match_on_str).unwrap_or_default();
            let mut triples = Vec::new();
            for item in items {
                if let Some(cat_part) = item.strip_prefix("CAT:") {
                    let cid: i64 = cat_part.parse().unwrap_or(0);
                    if cid != 0 {
                        let t = category_title(pool, cid as i32, uid).await;
                        triples.push(Value::Array(vec![
                            Value::String(t),
                            Value::Bool(true),
                            Value::Bool(false),
                        ]));
                    } else {
                        triples.push(Value::Array(vec![
                            Value::String("0".into()),
                            Value::Bool(true),
                            Value::Bool(true),
                        ]));
                    }
                } else {
                    let fid: i64 = item.parse().unwrap_or(0);
                    if fid != 0 {
                        let t = feed_title(pool, fid as i32, uid).await;
                        triples.push(Value::Array(vec![
                            Value::String(t),
                            Value::Bool(false),
                            Value::Bool(false),
                        ]));
                    } else {
                        triples.push(Value::Array(vec![
                            Value::String("0".into()),
                            Value::Bool(false),
                            Value::Bool(true),
                        ]));
                    }
                }
            }
            rule.insert("match".into(), Value::Array(triples));
        } else {
            // Legacy: `feed` = the feed (or category, when `cat_filter`) title, or "" if
            // the referenced id is missing — PHP: `(cat_filter && cat_id) || feed_id`.
            let has =
                (cat_filter && cat_id.is_some_and(|c| c != 0)) || feed_id.is_some_and(|f| f != 0);
            let title = if has {
                if cat_filter {
                    category_title(pool, cat_id.unwrap_or(0), uid).await
                } else {
                    feed_title(pool, feed_id.unwrap_or(0), uid).await
                }
            } else {
                String::new()
            };
            rule.insert("feed".into(), Value::String(title));
        }
        rules.push(Value::Object(rule));
    }
    obj.insert("rules".into(), Value::Array(rules));

    // --- actions ---
    let mut actions: Vec<Value> = Vec::new();
    let arows = sqlx::query(
        "SELECT action_id, action_param FROM ttrss_filters2_actions WHERE filter_id = $1",
    )
    .bind(filter_id)
    .fetch_all(pool)
    .await
    .expect("filter actions");
    for a in &arows {
        let mut action = Map::new();
        action.insert("action_id".into(), Value::from(a.get::<i32, _>(0)));
        action.insert("action_param".into(), Value::String(a.get::<String, _>(1)));
        actions.push(Value::Object(action));
    }
    obj.insert("actions".into(), Value::Array(actions));

    serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
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

    let name = sess.data().get_str("name").unwrap_or("user").replace(
        |c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "",
    );
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

// ---------------------------------------------------------------------------
// Import (OPML::opml_import / importOpml)
// ---------------------------------------------------------------------------

/// A parsed XML element (flat list; `children` are indices into the same list).
#[derive(Debug)]
struct XmlNode {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<usize>,
}

fn take_attrs(e: &quick_xml::events::BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| {
            // Entity-decode only (quick-xml's `value` is raw). Deliberate
            // deviation from the reference: PHP's DOM import also applies
            // XML attribute whitespace normalization (newlines → space),
            // which smushes multi-line pref values like USER_STYLESHEET —
            // the OPML PHP *exports* keeps their literal newlines, so we
            // keep them too. Unparseable entity → the raw value (old behavior).
            let v = match quick_xml::escape::unescape(&a.value) {
                Ok(u) => u.to_string(),
                Err(_) => a.value.to_string(),
            };
            (a.key.as_ref().to_string(), v)
        })
        .collect()
}

/// Parse an OPML document into a flat element tree. Returns the node list and
/// the index of the root element.
fn parse_opml(xml: &str) -> Result<(Vec<XmlNode>, usize), quick_xml::Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut nodes: Vec<XmlNode> = Vec::new();
    let mut stack: Vec<usize> = Vec::new(); // open element indices
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let idx = nodes.len();
                nodes.push(XmlNode {
                    name: e.local_name().as_ref().to_string(),
                    attrs: take_attrs(&e),
                    text: String::new(),
                    children: Vec::new(),
                });
                if let Some(&p) = stack.last() {
                    nodes[p].children.push(idx);
                }
                stack.push(idx);
            }
            Event::Empty(e) => {
                let idx = nodes.len();
                nodes.push(XmlNode {
                    name: e.local_name().as_ref().to_string(),
                    attrs: take_attrs(&e),
                    text: String::new(),
                    children: Vec::new(),
                });
                if let Some(&p) = stack.last() {
                    nodes[p].children.push(idx);
                }
            }
            Event::Text(e) => {
                if let Some(&p) = stack.last() {
                    nodes[p]
                        .text
                        .push_str(&e.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(e) => {
                if let Some(&p) = stack.last() {
                    nodes[p].text.push_str(&e);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok((nodes, stack.first().copied().unwrap_or(0)))
}

fn attr<'a>(node: &'a XmlNode, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The `outline` child elements of `idx`.
fn outline_children(nodes: &[XmlNode], idx: usize) -> Vec<usize> {
    nodes[idx]
        .children
        .iter()
        .copied()
        .filter(|&ci| nodes[ci].name == "outline")
        .collect()
}

/// The top-level `outline` elements under `opml/body` (the `//opml/body/outline` set).
fn body_outlines(nodes: &[XmlNode], root: usize) -> Vec<usize> {
    let body = nodes[root]
        .children
        .iter()
        .copied()
        .find(|&ci| nodes[ci].name == "body");
    body.map(|bi| outline_children(nodes, bi))
        .unwrap_or_default()
}

/// `mb_substr(s, 0, max)` — truncate to at most `max` characters (multibyte-safe).
fn mb_truncate(s: &str, max: usize) -> &str {
    let n = s.chars().take(max).count();
    match s.char_indices().nth(n) {
        Some((bi, _)) => &s[..bi],
        None => s,
    }
}

/// `OPML::get_feed_category` — id of the category with this title under
/// `parent_cat_id` (0 = top-level, i.e. `parent_cat IS NULL`), or 0 if absent.
async fn get_feed_category(db: &Db, title: &str, uid: i32, parent_cat_id: i32) -> i32 {
    let pool = db.pool();
    sqlx::query_scalar::<_, i32>(
        "SELECT id FROM ttrss_feed_categories WHERE title = $1 AND owner_uid = $2 AND (parent_cat = $3 OR ($3 = 0 AND parent_cat IS NULL))")
        .bind(title)
        .bind(uid)
        .bind(parent_cat_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `Feeds::_add_cat` — insert a category (the caller has confirmed it is absent).
async fn add_cat_low(
    db: &Db,
    title: &str,
    uid: i32,
    parent_cat: Option<i32>,
    order_id: i32,
) -> bool {
    let pool = db.pool();
    sqlx::query("INSERT INTO ttrss_feed_categories (owner_uid, parent_cat, order_id, title) VALUES ($1, $2, $3, $4)")
        .bind(uid)
        .bind(parent_cat)
        .bind(order_id)
        .bind(title)
        .execute(pool)
        .await
        .is_ok()
}

/// `OPML::opml_notice` — one line of the import response body: 3×`&nbsp;` per
/// depth level, the message, then `<br/>` (the CLI logs instead; only the HTTP
/// surface is ported here).
fn opml_notice(body: &mut String, msg: &str, nest: usize) {
    for _ in 0..nest {
        body.push_str("&nbsp;&nbsp;&nbsp;");
    }
    body.push_str(msg);
    body.push_str("<br/>");
}

/// `OPML::opml_import_feed` — insert a feed (deduped by `feed_url` + owner).
async fn opml_import_feed(
    db: &Db,
    uid: i32,
    node: &XmlNode,
    cat_id: i32,
    body: &mut String,
    depth: usize,
) {
    let pool = db.pool();
    let feed_title = attr(node, "text")
        .or_else(|| attr(node, "title"))
        .map(|t| mb_truncate(t, 250).to_string())
        .unwrap_or_default();
    let feed_url = match attr(node, "xmlUrl").or_else(|| attr(node, "xmlURL")) {
        Some(u) => u.to_string(),
        None => return,
    };
    let site_url = attr(node, "htmlUrl")
        .map(|t| mb_truncate(t, 250).to_string())
        .unwrap_or_default();
    let exists = sqlx::query_scalar::<_, i32>(
        "SELECT id FROM ttrss_feeds WHERE feed_url = $1 AND owner_uid = $2",
    )
    .bind(&feed_url)
    .bind(uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some();
    // PHP notices the feed by title, or the URL when the title is missing.
    let display = if feed_title.is_empty() {
        feed_url.as_str()
    } else {
        feed_title.as_str()
    };
    if exists {
        opml_notice(body, &t_sprintf("Duplicate feed: %s", &[display]), depth);
        return;
    }
    opml_notice(body, &t_sprintf("Adding feed: %s", &[display]), depth);
    let feed_title = if feed_title.is_empty() {
        "[Unknown]".to_string()
    } else {
        feed_title
    };
    // PHP casts these with (int) — lenient leading-numeric-prefix parse — not strict parse.
    let update_interval = attr(node, "ttrssUpdateInterval")
        .map(php_int_cast)
        .unwrap_or(0);
    let order_id: i32 = attr(node, "ttrssSortOrder").map(php_int_cast).unwrap_or(0) as i32;
    let purge_interval = attr(node, "ttrssPurgeInterval")
        .map(php_int_cast)
        .unwrap_or(0);
    let cat = (cat_id != 0).then_some(cat_id);
    sqlx::query(
        "INSERT INTO ttrss_feeds (title, feed_url, owner_uid, cat_id, site_url, order_id, update_interval, purge_interval) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(&feed_title)
        .bind(&feed_url)
        .bind(uid)
        .bind(cat)
        .bind(&site_url)
        .bind(order_id)
        .bind(update_interval)
        .bind(purge_interval)
        .execute(pool)
        .await
        .ok();
}

/// `OPML::opml_import_preference` — set a user pref from `pref-name` / `value`
/// (PHP writes to the active profile: `$_SESSION['profile'] ?? null`).
async fn opml_import_preference(
    db: &Db,
    uid: i32,
    profile: Option<i32>,
    node: &XmlNode,
    body: &mut String,
    depth: usize,
) {
    let pref_name = match attr(node, "pref-name") {
        Some(n) => n,
        None => return,
    };
    let pref_value = attr(node, "value").unwrap_or("");
    opml_notice(
        body,
        &t_sprintf("Setting preference key %s to %s", &[pref_name, pref_value]),
        depth,
    );
    crate::prefs::set(db, pref_name, pref_value, uid, profile, true).await;
}

/// `OPML::opml_import_label` — create a label from `label-name` / `label-fg-color`
/// / `label-bg-color` (skipped if a label with the caption already exists).
async fn opml_import_label(db: &Db, uid: i32, node: &XmlNode, body: &mut String, depth: usize) {
    let label_name = match attr(node, "label-name") {
        Some(n) => n,
        None => return,
    };
    if crate::labels::find_id(db.pool(), label_name, uid).await != 0 {
        opml_notice(
            body,
            &t_sprintf("Duplicate label: %s", &[&htmlspecialchars(label_name)]),
            depth,
        );
        return;
    }
    opml_notice(
        body,
        &t_sprintf("Adding label %s", &[&htmlspecialchars(label_name)]),
        depth,
    );
    crate::labels::create(
        db,
        label_name,
        attr(node, "label-fg-color"),
        attr(node, "label-bg-color"),
        uid,
        Some(uid),
    )
    .await
    .ok();
}

/// `Feeds::_find_by_title` — id of the feed (or category, if `is_cat`) with this
/// title for the owner, or 0.
async fn find_by_title(db: &Db, title: &str, is_cat: bool, uid: i32) -> i32 {
    let pool = db.pool();
    let sql = if is_cat {
        "SELECT id FROM ttrss_feed_categories WHERE owner_uid = $1 AND title = $2"
    } else {
        "SELECT id FROM ttrss_feeds WHERE owner_uid = $1 AND title = $2"
    };
    sqlx::query_scalar::<_, i32>(sql)
        .bind(uid)
        .bind(title)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `OPML::opml_import_filter` — insert a filter from a `filter-type="2"` outline
/// whose text content is a JSON filter definition.
async fn opml_import_filter(db: &Db, uid: i32, node: &XmlNode, body: &mut String, depth: usize) {
    let pool = db.pool();
    if attr(node, "filter-type") != Some("2") {
        return;
    }
    let filter: Value = match serde_json::from_str::<Value>(node.text.trim()) {
        Ok(v) if v.is_object() => v,
        _ => return,
    };
    let match_any_rule = filter
        .get("match_any_rule")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let enabled = filter
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let inverse = filter
        .get("inverse")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let title: &str = filter.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let filter_id: i32 = match sqlx::query_scalar::<_, i32>(
        "INSERT INTO ttrss_filters2 (match_any_rule, enabled, inverse, title, owner_uid) VALUES ($1, $2, $3, $4, $5) RETURNING id")
        .bind(match_any_rule).bind(enabled).bind(inverse).bind(title).bind(uid)
        .fetch_optional(pool).await.ok().flatten()
    {
        Some(id) => {
            opml_notice(body, &t_sprintf("Adding filter %s...", &[title]), depth);
            id
        }
        None => return,
    };
    if let Some(rules) = filter.get("rules").and_then(|v| v.as_array()) {
        for rule in rules {
            let match_arr = rule
                .get("match")
                .and_then(|v| v.as_array())
                .filter(|a| !a.is_empty());
            if let Some(m) = match_arr {
                let mut match_on: Vec<String> = Vec::new();
                for item in m {
                    let name = item.get(0).and_then(|v| v.as_str()).unwrap_or("");
                    let is_cat = item.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
                    let is_id = item.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
                    if is_id {
                        match_on.push(if is_cat {
                            format!("CAT:{name}")
                        } else {
                            name.to_string()
                        });
                    } else {
                        let mid = find_by_title(db, name, is_cat, uid).await;
                        if mid != 0 {
                            match_on.push(if is_cat {
                                format!("CAT:{mid}")
                            } else {
                                mid.to_string()
                            });
                        }
                    }
                }
                let reg_exp = rule.get("reg_exp").and_then(|v| v.as_str()).unwrap_or("");
                let ftype = rule
                    .get("filter_type")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let inv = rule
                    .get("inverse")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let match_on_json =
                    serde_json::to_string(&match_on).unwrap_or_else(|_| "[]".to_string());
                sqlx::query("INSERT INTO ttrss_filters2_rules (feed_id, cat_id, match_on, filter_id, filter_type, reg_exp, cat_filter, inverse) VALUES (NULL, NULL, $1, $2, $3, $4, false, $5)")
                    .bind(&match_on_json).bind(filter_id).bind(ftype).bind(reg_exp).bind(inv)
                    .execute(pool).await.ok();
            } else {
                let feed = rule.get("feed").and_then(|v| v.as_str()).unwrap_or("");
                let cat_filter = rule
                    .get("cat_filter")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mid = find_by_title(db, feed, cat_filter, uid).await;
                let feed_id = (mid != 0 && !cat_filter).then_some(mid);
                let cat_id = (mid != 0 && cat_filter).then_some(mid);
                let reg_exp = rule.get("reg_exp").and_then(|v| v.as_str()).unwrap_or("");
                let ftype = rule
                    .get("filter_type")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let inv = rule
                    .get("inverse")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                sqlx::query("INSERT INTO ttrss_filters2_rules (feed_id, cat_id, filter_id, filter_type, reg_exp, cat_filter, inverse) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                    .bind(feed_id).bind(cat_id).bind(filter_id).bind(ftype).bind(reg_exp).bind(cat_filter).bind(inv)
                    .execute(pool).await.ok();
            }
        }
    }
    if let Some(actions) = filter.get("actions").and_then(|v| v.as_array()) {
        for action in actions {
            // PHP `(int)$action["action_id"]` — a missing key casts null to 0.
            let action_id = action
                .get("action_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let action_param = action
                .get("action_param")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, $2, $3)")
                .bind(filter_id).bind(action_id).bind(action_param)
                .execute(pool).await.ok();
        }
    }
}

/// `OPML::opml_import_category` — walk the outline tree (iteratively, work stack
/// in place of the PHP recursion), creating categories and importing feeds / labels /
/// prefs / filters. A node with `text`/`title` and no `xmlUrl` is a category (or one
/// of the three settings sections); everything else is a feed or settings item.
async fn opml_import_category(
    db: &Db,
    uid: i32,
    profile: Option<i32>,
    nodes: &[XmlNode],
    root: usize,
    body: &mut String,
) {
    let default_cat_id = get_feed_category(db, "Imported feeds", uid, 0).await;
    let mut stack: Vec<(Vec<usize>, i32, Option<String>, usize)> =
        vec![(body_outlines(nodes, root), 0, None, 0)];
    while let Some((outlines, cat_id, cat_title, depth)) = stack.pop() {
        // `Processing category: …` — `Uncategorized` for the root call
        // (`$root_node = null` → `$cat_title = false`).
        let cat_display = cat_title.clone().unwrap_or_else(|| __("Uncategorized"));
        opml_notice(
            body,
            &t_sprintf("Processing category: %s", &[&cat_display]),
            depth,
        );
        for oi in outlines {
            let node = &nodes[oi];
            let node_cat_title = attr(node, "text").or_else(|| attr(node, "title"));
            let node_feed_url = attr(node, "xmlUrl").or_else(|| attr(node, "xmlURL"));
            // PHP: `$node_cat_title && !$node_feed_url` — a falsy (empty) title is a feed.
            if node_cat_title.is_some_and(|t| !t.is_empty()) && node_feed_url.is_none() {
                // PHP truncates the category title to 250 chars, falling back to
                // `title` only when the truncated `text` is falsy.
                let title = attr(node, "text")
                    .filter(|t| !t.is_empty())
                    .map(|t| mb_truncate(t, 250).to_string())
                    .or_else(|| attr(node, "title").map(|t| mb_truncate(t, 250).to_string()))
                    .unwrap_or_default();
                let is_settings = matches!(
                    title.as_str(),
                    "tt-rss-prefs" | "tt-rss-labels" | "tt-rss-filters"
                );
                let new_cat_id = if is_settings {
                    0
                } else if get_feed_category(db, &title, uid, cat_id).await != 0 {
                    get_feed_category(db, &title, uid, cat_id).await
                } else {
                    let order_id: i32 =
                        attr(node, "ttrssSortOrder").map(php_int_cast).unwrap_or(0) as i32;
                    add_cat_low(db, &title, uid, (cat_id != 0).then_some(cat_id), order_id).await;
                    get_feed_category(db, &title, uid, cat_id).await
                };
                stack.push((
                    outline_children(nodes, oi),
                    new_cat_id,
                    Some(title),
                    depth + 1,
                ));
            } else {
                let dst_cat_id = if cat_id != 0 { cat_id } else { default_cat_id };
                match cat_title.as_deref() {
                    Some("tt-rss-prefs") => {
                        opml_import_preference(db, uid, profile, node, body, depth + 1).await
                    }
                    Some("tt-rss-labels") => {
                        opml_import_label(db, uid, node, body, depth + 1).await
                    }
                    Some("tt-rss-filters") => {
                        opml_import_filter(db, uid, node, body, depth + 1).await
                    }
                    _ => opml_import_feed(db, uid, node, dst_cat_id, body, depth + 1).await,
                }
            }
        }
    }
}

/// `Pref_Feeds::importOpml` / `OPML::opml_import` — import the uploaded OPML (the
/// bytes of the multipart `opml_file` field) and return the response body: the
/// `opml_notice` lines, or `print_error`'s `format_error` div when no file was
/// uploaded (PHP `$_FILES['opml_file']['error']` = UPLOAD_ERR_NO_FILE, 4).
pub async fn import(state: &AppState, sess: &Sess, file: Option<&[u8]>) -> String {
    let uid = sess.data().uid().unwrap_or(0);
    if uid == 0 {
        return String::new();
    }
    let Some(bytes) = file else {
        // PHP `$_FILES['opml_file']['error']` = 4 (UPLOAD_ERR_NO_FILE).
        return format_error(&t_sprintf("Upload failed with error code %d", &["4"]), "");
    };
    // PHP `OPML::opml_import_preference` writes to `$_SESSION['profile'] ?? null`.
    let profile = sess.data().profile().map(|p| p as i32);
    let text = String::from_utf8_lossy(bytes);
    let mut body = String::new();
    match parse_opml(&text) {
        Ok((nodes, root)) => {
            opml_import_category(&state.db, uid, profile, &nodes, root, &mut body).await;
        }
        Err(_) => {
            opml_notice(&mut body, &__("Error while parsing document."), 0);
        }
    }
    // `OPML::opml_import` → `Prefs::set` refreshes the in-process cache for the
    // active profile (PHP OPML.php:331 writes to `$_SESSION['profile'] ?? null`).
    crate::prefs::invalidate(&state.prefs_cache, uid, profile);
    body
}
