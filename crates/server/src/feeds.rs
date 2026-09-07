//! Port of `Pref_Feeds::getfeedtree` + the `Feeds` counter / icon / title
//! helpers it relies on — the feed list the Dojo SPA renders in the left
//! panel.
//!
//! `backend.php?op=Pref_Feeds&method=getfeedtree&mode=2` (a GET, CSRF-ignored)
//! returns a nested dojo `ItemFileReadStore`: `{identifier, label, items:[…]}`.
//! Mode 2 (the SPA's default) lays out the special + label categories and the
//! real categories as a flat top-level list (no `root` node); mode 0 wraps
//! everything in a single `root` node.

use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::prefs;
use crate::time::TzCtx;
use crate::session::Sess;
use crate::AppState;

// `Feeds::*` / `LABEL_BASE_INDEX` (functions.php).
const LABEL_BASE_INDEX: i64 = -1024;
const FEED_ARCHIVED: i64 = 0;
const FEED_STARRED: i64 = -1;
const FEED_PUBLISHED: i64 = -2;
const FEED_FRESH: i64 = -3;
const FEED_ALL: i64 = -4;
const FEED_RECENTLY_READ: i64 = -6;
const CAT_SPECIAL: i64 = -1;
const CAT_LABELS: i64 = -2;

/// `Labels::label_to_feed_id` = `LABEL_BASE_INDEX - 1 - abs(label)`.
fn label_to_feed_id(label: i64) -> i64 {
    LABEL_BASE_INDEX - 1 - label.abs()
}

/// `Labels::feed_to_label_id` (inverse; positive for label feed ids).
fn feed_to_label_id(feed: i64) -> i64 {
    -1025 - feed
}

/// `clean($_REQUEST['mode'] ?? 0) == 2`.
fn is_mode_2(extra: &Map<String, Value>) -> bool {
    extra
        .get("mode")
        .and_then(|v| v.as_str().unwrap_or("").trim().parse::<i64>().ok())
        == Some(2)
}

/// `Pref_Feeds::feedsWithErrors` — the feeds with a non-empty `last_error`.
/// No `ORDER BY`: the natural plan order is what the oracle emits.
pub async fn feeds_with_errors(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let rows = sqlx::query(
        "SELECT id, title, feed_url, last_error, site_url FROM ttrss_feeds \n\n         WHERE last_error != '' AND owner_uid = $1 AND update_interval >= 0",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i32, _>(0) as i64,
                "title": r.get::<String, _>(1),
                "feed_url": r.get::<String, _>(2),
                "last_error": r.get::<String, _>(3),
                "site_url": r.get::<Option<String>, _>(4).unwrap_or_default(),
            })
        })
        .collect();
    Value::Array(items)
}

/// `Handler::_param_to_bool`.
fn param_to_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => {
            let t = s.trim();
            !t.is_empty() && t != "f" && t != "false"
        }
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

/// A `ttrss_feed_categories` row (ordered by `order_id`, `title`).
struct CatRow {
    id: i64,
    title: String,
    parent_cat: Option<i64>,
}

/// A `ttrss_feeds` row + its article count (ordered by `order_id`, `title`).
struct FeedRow {
    id: i64,
    title: String,
    last_error: String,
    update_interval: i64,
    last_updated: Option<String>,
    num_articles: i64,
    cat_id: Option<i64>,
}

/// `Pref_Feeds::inactiveFeeds` — the feeds whose latest article is older than
/// 3 months. The PHP does `foreach ($feeds as $f) { $f['last_article'] = ... }`
/// **by value**, so the `make_local_datetime` reassignment is discarded and the
/// raw `MAX(e.updated)` timestamp string is what gets emitted. We reproduce that
/// faithfully (a `to_char` of the stored UTC timestamp, `YYYY-MM-DD HH24:MI:SS`).
pub async fn inactive_feeds(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let rows = sqlx::query(
        "SELECT f.id, f.title, f.site_url, f.feed_url, \n         to_char(MAX(e.updated), 'YYYY-MM-DD HH24:MI:SS') AS last_article\n         FROM ttrss_feeds AS f\n         JOIN ttrss_user_entries AS ue ON ue.feed_id = f.id\n         JOIN ttrss_entries AS e ON e.id = ue.ref_id\n         WHERE f.owner_uid = $1\n           AND (SELECT MAX(ttrss_entries.updated) FROM ttrss_entries\n                JOIN ttrss_user_entries\n                    ON ttrss_entries.id = ttrss_user_entries.ref_id\n                WHERE ttrss_user_entries.feed_id = f.id)\n                < NOW() - INTERVAL '3 months'\n         GROUP BY f.title, f.id, f.site_url, f.feed_url\n         ORDER BY MAX(e.updated) ASC",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i32, _>(0) as i64,
                "title": r.get::<String, _>(1),
                "site_url": r.get::<Option<String>, _>(2),
                "feed_url": r.get::<String, _>(3),
                "last_article": r.get::<Option<String>, _>(4),
            })
        })
        .collect();
    Value::Array(items)
}

/// `Pref_Feeds::getfeedtree` → `_makefeedtree()`.
pub async fn getfeedtree(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let pool = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let enable_cats = prefs::resolve("ENABLE_FEED_CATS", &rows, profile).as_bool();
    let fresh_max_age = prefs::resolve("FRESH_ARTICLE_MAX_AGE", &rows, profile).as_int();
    let tz = TzCtx {
        user_tz: prefs::resolve("USER_TIMEZONE", &rows, profile).as_str().to_string(),
        client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
        short_date_format: prefs::resolve("SHORT_DATE_FORMAT", &rows, profile).as_str().to_string(),
        long_date_format: prefs::resolve("LONG_DATE_FORMAT", &rows, profile).as_str().to_string(),
    };

    let mode_is_2 = is_mode_2(extra);
    let search = if mode_is_2 {
        extra
            .get("search")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        String::new() // `$_SESSION["prefs_feed_search"] ?? ""`
    };
    let force_show_empty = extra
        .get("force_show_empty")
        .map(param_to_bool)
        .unwrap_or(false);

    let cats = fetch_all_cats(pool, uid).await;
    let feeds = fetch_all_feeds(pool, uid).await;

    let mut root_items: Vec<Value> = Vec::new();
    let mut root_children = 0i64;

    // ---- Mode 2: special + label "categories" first. ----
    if mode_is_2 {
        // `feedlist_init_cat(CATEGORY_SPECIAL)` + the six virtual feeds.
        let mut special = Map::new();
        special.insert("id".into(), json!(format!("CAT:{CAT_SPECIAL}")));
        special.insert("items".into(), Value::Array(Vec::new()));
        special.insert("name".into(), json!(cat_title(CAT_SPECIAL)));
        special.insert("type".into(), json!("category"));
        special.insert("unread".into(), json!(-1));
        special.insert("bare_id".into(), json!(CAT_SPECIAL));
        for feed_id in [
            FEED_ALL,
            FEED_FRESH,
            FEED_STARRED,
            FEED_PUBLISHED,
            FEED_ARCHIVED,
            FEED_RECENTLY_READ,
        ] {
            special
                .get_mut("items")
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(special_feed(pool, uid, feed_id, fresh_max_age).await);
        }
        // (Plugin feeds for CATEGORY_SPECIAL — none for this deployment.)
        if enable_cats {
            root_items.push(Value::Object(special));
        }

        // Labels category.
        let labels = sqlx::query(
            "SELECT id, fg_color, bg_color, caption FROM ttrss_labels2
             WHERE owner_uid = $1 ORDER BY caption",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        if !labels.is_empty() {
            let mut label_cat: Map<String, Value> = if enable_cats {
                let mut m = Map::new();
                m.insert("id".into(), json!(format!("CAT:{CAT_LABELS}")));
                m.insert("items".into(), Value::Array(Vec::new()));
                m.insert("name".into(), json!(cat_title(CAT_LABELS)));
                m.insert("type".into(), json!("category"));
                m.insert("unread".into(), json!(-1));
                m.insert("bare_id".into(), json!(CAT_LABELS));
                m
            } else {
                let mut m = Map::new();
                m.insert("items".into(), Value::Array(Vec::new()));
                m
            };
            for r in &labels {
                let label_id = r.get::<i32, _>(0) as i64;
                let mut feed =
                    special_feed(pool, uid, label_to_feed_id(label_id), fresh_max_age).await;
                if let Value::Object(obj) = &mut feed {
                    obj.insert("name".into(), json!(r.get::<String, _>(3)));
                    obj.insert("fg_color".into(), json!(r.get::<String, _>(1)));
                    obj.insert("bg_color".into(), json!(r.get::<String, _>(2)));
                }
                label_cat
                    .get_mut("items")
                    .and_then(Value::as_array_mut)
                    .unwrap()
                    .push(feed);
            }
            if enable_cats {
                root_items.push(Value::Object(label_cat));
            }
        }
    }

    // ---- Real categories (and uncategorized) — same for modes 0 and 2. ----
    if enable_cats {
        let show_empty_cats = force_show_empty || (!mode_is_2 && search.is_empty());

        // Top-level categories (`parent_cat IS NULL`), ordered (order_id, title).
        for c in cats.iter().filter(|c| c.parent_cat.is_none()) {
            let items = build_cat_items(&cats, &feeds, state, c.id, show_empty_cats, &tz);
            let n = calculate_children_count(&items);
            let mut cat = Map::new();
            cat.insert("id".into(), json!(format!("CAT:{}", c.id)));
            cat.insert("bare_id".into(), json!(c.id));
            cat.insert("auxcounter".into(), json!(-1));
            cat.insert("name".into(), json!(c.title.clone()));
            cat.insert("items".into(), Value::Array(items));
            cat.insert("checkbox".into(), json!(false));
            cat.insert("type".into(), json!("category"));
            cat.insert("unread".into(), json!(-1));
            cat.insert("child_unread".into(), json!(-1));
            cat.insert("param".into(), json!(nfeeds(n)));
            if n > 0 || show_empty_cats {
                root_children += n;
                root_items.push(Value::Object(cat));
            }
        }

        // Uncategorized (a special case: `cat_id IS NULL`).
        let items: Vec<Value> = feeds
            .iter()
            .filter(|f| f.cat_id.is_none())
            .map(|f| feed_node(state, f, &tz))
            .collect();
        let n = items.len() as i64;
        if n > 0 || show_empty_cats {
            root_children += n;
            let mut unc = Map::new();
            unc.insert("id".into(), json!("CAT:0"));
            unc.insert("bare_id".into(), json!(0i64));
            unc.insert("auxcounter".into(), json!(-1));
            unc.insert("name".into(), json!("Uncategorized"));
            unc.insert("items".into(), Value::Array(items));
            unc.insert("type".into(), json!("category"));
            unc.insert("checkbox".into(), json!(false));
            unc.insert("unread".into(), json!(-1));
            unc.insert("child_unread".into(), json!(-1));
            unc.insert("param".into(), json!(nfeeds(n)));
            root_items.push(Value::Object(unc));
        }
    } else {
        // Cats disabled: every feed directly under the root.
        root_items = feeds.iter().map(|f| feed_node(state, f, &tz)).collect();
        root_children = feeds.len() as i64;
    }

    let items = if mode_is_2 {
        Value::Array(root_items)
    } else {
        let mut root = Map::new();
        root.insert("id".into(), json!("root"));
        root.insert("name".into(), json!("Feeds"));
        root.insert("items".into(), Value::Array(root_items));
        root.insert("param".into(), json!(nfeeds(root_children)));
        root.insert("type".into(), json!("category"));
        Value::Array(vec![Value::Object(root)])
    };

    let mut out = Map::new();
    out.insert("identifier".into(), json!("id"));
    out.insert("label".into(), json!("name"));
    out.insert("items".into(), items);
    Value::Object(out)
}

/// `Feeds::_get_cat_title` for the special/uncategorized ids (real category
/// titles come from the DB rows).
fn cat_title(cat_id: i64) -> &'static str {
    match cat_id {
        CAT_SPECIAL => "Special",
        CAT_LABELS => "Labels",
        _ => "Uncategorized",
    }
}

/// A special-feed or label node: `feedlist_init_feed(feed_id)`.
async fn special_feed(
    pool: &sqlx::PgPool,
    uid: i64,
    feed_id: i64,
    fresh_max_age: i64,
) -> Value {
    let unread = counters(pool, uid, feed_id, fresh_max_age).await;
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("FEED:{feed_id}")));
    m.insert("name".into(), json!(special_title(feed_id)));
    m.insert("unread".into(), json!(unread));
    m.insert("type".into(), json!("feed"));
    m.insert("error".into(), json!(""));
    m.insert("updated".into(), json!(""));
    m.insert("icon".into(), json!(special_icon(feed_id)));
    m.insert("bare_id".into(), json!(feed_id));
    m.insert("auxcounter".into(), json!(0));
    Value::Object(m)
}

/// `Feeds::_get_title` for the virtual feeds.
fn special_title(feed_id: i64) -> &'static str {
    match feed_id {
        FEED_STARRED => "Starred articles",
        FEED_PUBLISHED => "Published articles",
        FEED_FRESH => "Fresh articles",
        FEED_ALL => "All articles",
        FEED_ARCHIVED => "Archived articles",
        FEED_RECENTLY_READ => "Recently read",
        _ => "",
    }
}

/// `Feeds::_get_title($id, $owner_uid)` (the non-cat branch): virtual feeds,
/// labels (with the `Unknown label (n)` fallback) and real feeds.
pub async fn feed_title(pool: &sqlx::PgPool, id: i64, owner_uid: i64) -> String {
    match id {
        FEED_STARRED => "Starred articles".to_string(),
        FEED_PUBLISHED => "Published articles".to_string(),
        FEED_FRESH => "Fresh articles".to_string(),
        FEED_ALL => "All articles".to_string(),
        FEED_ARCHIVED => "Archived articles".to_string(),
        FEED_RECENTLY_READ => "Recently read".to_string(),
        _ if id < LABEL_BASE_INDEX => {
            let label_id = crate::labels::feed_to_label_id(id);
            sqlx::query_scalar::<_, String>(
                "SELECT caption FROM ttrss_labels2 WHERE id = $1 AND owner_uid = $2 LIMIT 1",
            )
            .bind(label_id as i32)
            .bind(owner_uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("Unknown label ({})", label_id))
        }
        _ if id > 0 => {
            sqlx::query_scalar::<_, String>(
                "SELECT title FROM ttrss_feeds WHERE id = $1 AND owner_uid = $2 LIMIT 1",
            )
            .bind(id as i32)
            .bind(owner_uid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("Unknown feed ({})", id))
        }
        _ => id.to_string(),
    }
}

/// `Feeds::updatedebugger` — the standalone feed-update debug page
/// (`Content-Type: text/html`). Reproduced verbatim from the PHP template;
/// the cache-bust timestamp and the csrf token normalize away in the diff.
const UPDATE_DEBUGGER_TEMPLATE: &str = r#"		<!DOCTYPE html>
		<html>
		<head>
			<title>Feed Debugger</title>
			<style type='text/css'>
				@media (prefers-color-scheme: dark) {
					body {
						background : #222;
					}
				}
				body.css_loading * {
					display : none;
				}

				.feed-xml {
					color : green;
				}

				.log-timestamp {
					color : gray;
				}

				.log-timestamp::before {
					content: "["
				}

				.log-timestamp::after {
					content: "]"
				}

			</style>
			<script>
				dojoConfig = {
					async: true,
					cacheBust: "1788113929",
					packages: [
						{ name: "fox", location: "../../js" },
					]
				};
			</script>
			<script src="js/utility.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="js/common.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="lib/dojo/dojo.js?1788113929" type="text/javascript" charset="utf-8"></script>
			<script src="lib/dojo/tt-rss-layer.js?1788113929" type="text/javascript" charset="utf-8"></script>
					</head>
		<body class="flat ttrss_utility feed_debugger css_loading">
		<script type="text/javascript">
			require(['dojo/parser', "dojo/ready", 'dijit/form/Button','dijit/form/CheckBox', 'fox/form/Select', 'dijit/form/Form',
				'dijit/form/Select','dijit/form/TextBox','dijit/form/ValidationTextBox'],function(parser, ready){
				ready(function() {
					parser.parse();
				});
			});
		</script>

			<div class="container">
				<h1>Feed Debugger: @FEEDID@: @TITLE@</h1>
				<div class="content">
					<form method="post" action="" dojoType="dijit.form.Form">
						<input dojoType="dijit.form.TextBox" style="display : none"
                name="op"
               value="Feeds">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="method"
               value="updatedebugger">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="csrf_token"
               value="@CSRF@">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="action"
               value="do_update">						<input dojoType="dijit.form.TextBox" style="display : none"
                name="feed_id"
               value="@FEEDID@">
						<fieldset>
							<label>
							<select dojoType='fox.form.Select' name="xdebug"
         id="" name="xdebug" ><option value="1" selected="selected">LOG_VERBOSE</option><option value="2" >LOG_EXTENDED</option></select></label>
						</fieldset>

						<fieldset>
							<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="force_refetch"
                     id=""> Force refetch</label>
						</fieldset>

						<fieldset class="narrow">
							<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="force_rehash"
                     id=""> Force rehash</label>
						</fieldset>

						<fieldset class="narrow">
							<label class="checkbox"><input dojoType='dijit.form.CheckBox' name="dump_feed_xml"
                     id=""> Dump feed XML</label>
						</fieldset>

						<button dojoType="dijit.form.Button" class="alt-primary" type="submit">Continue</button>					</form>

					<hr>

					<pre></pre>
				</div>
			</div>
		</body>
		</html>
		"#;

/// `Feeds::updatedebugger` — the standalone feed-update debug page.
pub async fn updatedebugger(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
    csrf: &str,
) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let feed_id = crate::util::clean_int(extra.get("feed_id"));
    let title = feed_title(pool, feed_id, uid).await;
    UPDATE_DEBUGGER_TEMPLATE
        .replace("@CSRF@", csrf)
        .replace("@FEEDID@", &feed_id.to_string())
        .replace("@TITLE@", &title)
}

/// `Feeds::_get_cat_title($cat_id, $owner_uid)`.
pub async fn category_title(pool: &sqlx::PgPool, cat_id: i64, owner_uid: i64) -> String {
    match cat_id {
        0 => "Uncategorized".to_string(),
        -1 => "Special".to_string(),
        -2 => "Labels".to_string(),
        _ => sqlx::query_scalar::<_, String>(
            "SELECT title FROM ttrss_feed_categories \n             WHERE id = $1 AND owner_uid = $2 LIMIT 1",
        )
        .bind(cat_id as i32)
        .bind(owner_uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "UNKNOWN".to_string()),
    }
}
/// `Feeds::_get_icon` for the virtual feeds.
fn special_icon(feed_id: i64) -> &'static str {
    match feed_id {
        FEED_ARCHIVED => "archive",
        FEED_STARRED => "star",
        FEED_PUBLISHED => "rss_feed",
        FEED_FRESH => "whatshot",
        FEED_ALL => "inbox",
        FEED_RECENTLY_READ => "restore",
        _ if feed_id < LABEL_BASE_INDEX => "label",
        _ => "",
    }
}

/// `Pref_Feeds::get_category_items(cat_id)` — recursive: nested subcategories
/// (with `parent_id`) then the feeds in this category, both already ordered
/// `(order_id, title)` by the fetch. Empty subcategories are dropped unless
/// `show_empty_cats` (mode 0 / `force_show_empty`).
fn build_cat_items(
    cats: &[CatRow],
    feeds: &[FeedRow],
    state: &AppState,
    cat_id: i64,
    show_empty_cats: bool,
    tz: &TzCtx,
) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();

    for s in cats.iter().filter(|c| c.parent_cat == Some(cat_id)) {
        let sub_items = build_cat_items(cats, feeds, state, s.id, show_empty_cats, tz);
        let n = calculate_children_count(&sub_items);
        let mut cat = Map::new();
        cat.insert("id".into(), json!(format!("CAT:{}", s.id)));
        cat.insert("bare_id".into(), json!(s.id));
        cat.insert("name".into(), json!(s.title.clone()));
        cat.insert("items".into(), Value::Array(sub_items));
        cat.insert("checkbox".into(), json!(false));
        cat.insert("type".into(), json!("category"));
        cat.insert("unread".into(), json!(-1));
        cat.insert("child_unread".into(), json!(-1));
        cat.insert("auxcounter".into(), json!(-1));
        cat.insert("parent_id".into(), json!(cat_id));
        cat.insert("param".into(), json!(nfeeds(n)));
        if n > 0 || show_empty_cats {
            items.push(Value::Object(cat));
        }
    }

    for f in feeds.iter().filter(|f| f.cat_id == Some(cat_id)) {
        items.push(feed_node(state, f, tz));
    }

    items
}

/// A regular feed node (category feed, uncategorized, or flat list).
fn feed_node(state: &AppState, f: &FeedRow, tz: &TzCtx) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(format!("FEED:{}", f.id)));
    out.insert("bare_id".into(), json!(f.id));
    out.insert("auxcounter".into(), json!(-1));
    out.insert("name".into(), json!(f.title.clone()));
    out.insert("checkbox".into(), json!(false));
    out.insert("unread".into(), json!(-1));
    out.insert("error".into(), json!(f.last_error.clone()));
    out.insert("icon".into(), json!(feed_icon_url(state, f.id)));
    out.insert(
        "param".into(),
        json!(articles_param(f.num_articles, &crate::time::make_local_datetime(f.last_updated.as_deref(), false, tz))),
    );
    out.insert(
        "updates_disabled".into(),
        json!((f.update_interval < 0) as i64),
    );
    Value::Object(out)
}

/// `Feeds::_get_icon_url(feed_id)` — the `public.php?op=feed_icon` URL if the
/// favicon exists, else `""`.
fn feed_icon_url(state: &AppState, feed_id: i64) -> String {
    let dir = &state.config.feed_icon_dir;
    if dir.join(feed_id.to_string()).is_file() {
        format!(
            "{}/public.php?op=feed_icon&id={feed_id}",
            state.config.self_url_rtrim()
        )
    } else {
        String::new()
    }
}

/// `calculate_children_count` — recursive count of feed (non-category) nodes.
fn calculate_children_count(items: &[Value]) -> i64 {
    let mut c = 0i64;
    for child in items {
        if child.get("type").and_then(Value::as_str) == Some("category") {
            if let Some(sub) = child.get("items").and_then(Value::as_array) {
                c += calculate_children_count(sub);
            }
        } else {
            c += 1;
        }
    }
    c
}

/// `_ngettext("(%d article / %s)", "(%d articles / %s)", n)` + the datetime.
fn articles_param(n: i64, hm: &str) -> String {
    let unit = if n == 1 { "article" } else { "articles" };
    format!("({n} {unit} / {hm})")
}

/// `_ngettext("(%d feed)", "(%d feeds)", n)`.
fn nfeeds(n: i64) -> String {
    let unit = if n == 1 { "feed" } else { "feeds" };
    format!("({n} {unit})")
}

/// All of the user's feed categories, ordered `(order_id, title)`.
async fn fetch_all_cats(pool: &sqlx::PgPool, uid: i64) -> Vec<CatRow> {
    let rows = sqlx::query(
        "SELECT id, title, parent_cat FROM ttrss_feed_categories
         WHERE owner_uid = $1 ORDER BY order_id, title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| CatRow {
            id: r.get::<i32, _>(0) as i64,
            title: r.get::<String, _>(1),
            parent_cat: r
                .try_get::<Option<i32>, _>(2)
                .ok()
                .flatten()
                .map(|p| p as i64),
        })
        .collect()
}

/// All of the user's feeds (+ article count), ordered `(order_id, title)`.
async fn fetch_all_feeds(pool: &sqlx::PgPool, uid: i64) -> Vec<FeedRow> {
    let rows = sqlx::query(
        "SELECT f.id, f.title, COALESCE(f.last_error, ''), f.update_interval,
                SUBSTRING_FOR_DATE(f.last_updated, 1, 19) AS last_updated,
                COUNT(ue.int_id) AS num_articles, f.cat_id
         FROM ttrss_feeds f
         LEFT JOIN ttrss_user_entries ue ON ue.feed_id = f.id
         WHERE f.owner_uid = $1
         GROUP BY f.id, f.cat_id
         ORDER BY f.order_id, f.title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| FeedRow {
            id: r.get::<i32, _>(0) as i64,
            title: r.get::<String, _>(1),
            last_error: r.get::<String, _>(2),
            update_interval: r.get::<i32, _>(3) as i64,
            last_updated: r.get::<Option<String>, _>(4),
            num_articles: r.get::<i64, _>(5),
            cat_id: r
                .try_get::<Option<i32>, _>(6)
                .ok()
                .flatten()
                .map(|c| c as i64),
        })
        .collect()
}

/// `Feeds::_get_counters(feed, is_cat=false, unread_only=true, uid)`.
async fn counters(
    pool: &sqlx::PgPool,
    uid: i64,
    feed_id: i64,
    fresh_max_age: i64,
) -> i64 {
    if feed_id == FEED_RECENTLY_READ {
        return 0;
    }
    // Labels (`feed < LABEL_BASE_INDEX`).
    if feed_id < LABEL_BASE_INDEX {
        let label_id = feed_to_label_id(feed_id);
        return sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM ttrss_user_entries ue
             JOIN ttrss_user_labels2 ul2 ON ul2.article_id = ue.ref_id
             WHERE ue.unread = true AND ue.owner_uid = $1 AND ul2.label_id = $2",
        )
        .bind(uid)
        .bind(label_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    }

    let (match_part, need_entries) = match feed_id {
        FEED_STARRED => ("marked = true".to_string(), false),
        FEED_PUBLISHED => ("published = true".to_string(), false),
        FEED_FRESH => (
            format!(
                "unread = true AND score >= 0 AND date_entered > NOW() - INTERVAL '{fresh_max_age} hour'"
            ),
            true,
        ),
        FEED_ALL => ("true".to_string(), false),
        n if n >= 0 => {
            if n == FEED_ARCHIVED {
                ("feed_id IS NULL".to_string(), false)
            } else {
                (format!("feed_id = {n}"), false)
            }
        }
        _ => ("true".to_string(), false),
    };

    let sql = if need_entries {
        format!(
            "SELECT count(int_id) FROM ttrss_user_entries, ttrss_entries
             WHERE unread = true
               AND ttrss_entries.id = ttrss_user_entries.ref_id
               AND ({match_part})
               AND ttrss_user_entries.owner_uid = $1"
        )
    } else {
        format!(
            "SELECT count(int_id) FROM ttrss_user_entries
             WHERE unread = true AND ({match_part}) AND ttrss_user_entries.owner_uid = $1"
        )
    };
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(uid)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}
