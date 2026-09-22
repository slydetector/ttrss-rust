//! DB tests for the `Pref_Filters::editrule` feed multi-select and its
//! `_feed_multi_select` helper (M4). The expected HTML is byte-for-byte the
//! PHP oracle's output for the same nested dataset (see
//! `oracle/recorders/feed_multi_select.php`) extended with the template seed's
//! owner-1 rows (`seed.sql`), covering the tree ordering, the
//! U+2003 em-space indentation, the `CAT:<id>` / feed-id / "All feeds" /
//! "Uncategorized" selection, and the `<select multiple=<TAB>rue"` opening quirk.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

const EM: char = '\u{2003}';
const ATTRS: &str =
    "required=\"1\" style=\"width : 100%; height : 300px\" dojoType=\"fox.form.ValidationMultiSelect\"";

/// The fixed nested dataset (owner 1). IDs sit in the 900/9010 range: the
/// template seed already owns category IDs 1–2 and feed IDs 1–4 for owner 1
/// (`seed.sql`), so the fixture must not collide.
///   categories: 901 "Zeta" (top), 902 "Alpha" (top), 903 "Mid" (child of 902),
///               904 "Deep" (child of 903)
///   feeds:      9010 "Feed A2" (cat 902), 9011 "Feed A1" (cat 902),
///               9012 "Feed M" (cat 903), 9013 "Feed D" (cat 904),
///               9014 "UnCat 1" (NULL), 9015 "UnCat 2" (NULL),
///               9016 "TopZ" (cat 901)
async fn insert_nested_dataset(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO ttrss_feed_categories (id, owner_uid, title, parent_cat) VALUES \
         (901, 1, 'Zeta', NULL), (902, 1, 'Alpha', NULL), (903, 1, 'Mid', 902), (904, 1, 'Deep', 903)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ttrss_feeds (id, owner_uid, title, feed_url, cat_id) VALUES \
         (9010, 1, 'Feed A2', 'u10', 902), (9011, 1, 'Feed A1', 'u11', 902), \
         (9012, 1, 'Feed M', 'u12', 903), (9013, 1, 'Feed D', 'u13', 904), \
         (9014, 1, 'UnCat 1', 'u14', NULL), (9015, 1, 'UnCat 2', 'u15', NULL), \
         (9016, 1, 'TopZ', 'u16', 901)",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The full nested multi-select (no selection): the fixture dataset plus the
/// template seed's owner-1 rows (categories 1–2, feeds 1–4 from `seed.sql`),
/// with the U+2003 em-space (`EM`) indentation and the tab in
/// `<select multiple=<TAB>rue"`.
fn expected_nested(selected: &[&str]) -> String {
    // Returns `"<option  value=...>..."` (two spaces) when the value is not
    // selected, else `"<option selected=\"1\" value=...>..."`.
    let opt = |value: &str, title: &str, selected: bool| -> String {
        let sel = if selected { "selected=\"1\"" } else { "" };
        format!("<option {sel} value={value}>{title}</option>")
    };
    let in_ = |v: &str| selected.contains(&v);
    let body = [
        opt("\"0\"", "All feeds", in_("0")),
        opt("'CAT:902'", "Alpha", in_("CAT:902")),
        opt("'CAT:903'", &format!("{EM}Mid"), in_("CAT:903")),
        opt("'CAT:904'", &format!("{EM}{EM}Deep"), in_("CAT:904")),
        opt("'9013'", &format!("{EM}{EM}{EM}Feed D"), in_("9013")),
        opt("'9012'", &format!("{EM}{EM}Feed M"), in_("9012")),
        opt("'9011'", &format!("{EM}Feed A1"), in_("9011")),
        opt("'9010'", &format!("{EM}Feed A2"), in_("9010")),
        // The template seed's owner-1 rows, interleaved in title order.
        opt("'CAT:2'", "Science &amp; Nature", in_("CAT:2")),
        opt("'4'", &format!("{EM}MIT Tech Review"), in_("4")),
        opt("'CAT:1'", "Technology", in_("CAT:1")),
        opt("'3'", &format!("{EM}Ars Technica"), in_("3")),
        opt("'1'", &format!("{EM}Hacker News"), in_("1")),
        opt("'2'", &format!("{EM}The Register"), in_("2")),
        opt("'CAT:901'", "Zeta", in_("CAT:901")),
        opt("'9016'", &format!("{EM}TopZ"), in_("9016")),
        opt("'CAT:0'", "Uncategorized", in_("CAT:0")),
        opt("'9014'", &format!("{EM}UnCat 1"), in_("9014")),
        opt("'9015'", &format!("{EM}UnCat 2"), in_("9015")),
    ]
    .concat();
    format!(
        "<select multiple={tab}rue\" id=\"feed_id\" name=\"feed_id\" {attrs}>{body}</select>",
        tab = '\t',
        attrs = ATTRS
    )
}

fn test_config() -> Config { common::test_config() }

async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(&url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// `editrule` with no selection: the full category tree (title order, each
/// category then its whole subtree then its feeds), the "Uncategorized"
/// section, and the `<select multiple=<TAB>rue"` opening — byte-for-byte the PHP.
#[tokio::test]
async fn test_editrule_nested() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    insert_nested_dataset(state.db.pool()).await;

    let result = ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!(""))])).await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&[]);
    assert_eq!(got, &want, "nested multi-select mismatch");
        Ok(())
    })).await.unwrap();
}

/// `editrule` with a selection: `0` (All feeds), feed `9011`, and `CAT:903` get
/// `selected="1"`.
#[tokio::test]
async fn test_editrule_selected() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    insert_nested_dataset(state.db.pool()).await;

    let result =
        ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("0,9011,CAT:903"))])).await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&["0", "9011", "CAT:903"]);
    assert_eq!(got, &want, "selected multi-select mismatch");
        Ok(())
    })).await.unwrap();
}

/// `editrule` with category + feed selection: `CAT:902`, `CAT:904`, and feed `9010`.
#[tokio::test]
async fn test_editrule_cat_selected() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    insert_nested_dataset(state.db.pool()).await;

    let result =
        ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("CAT:902,CAT:904,9010"))]))
            .await;
    let got = result["multiselect"].as_str().unwrap();
    let want = expected_nested(&["CAT:902", "CAT:904", "9010"]);
    assert_eq!(got, &want, "cat-selected multi-select mismatch");
        Ok(())
    })).await.unwrap();
}

/// `editrule` with `ENABLE_FEED_CATS` disabled: a flat, title-ordered feed list
/// (no "All feeds", no "Uncategorized", no indentation) inside the same `<select>`.
#[tokio::test]
async fn test_editrule_flat() {
    let _ = with_test_db(|url| Box::pin(async move {
    let (state, sess) = setup(&url).await;
    let db = state.db.pool();
    insert_nested_dataset(db).await;
    // Disable feed categories (a user pref override for the default profile).
    sqlx::query(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         VALUES (1, 'ENABLE_FEED_CATS', NULL, 'false')",
    )
    .execute(db)
    .await
    .unwrap();

    let result = ttrss::filters::editrule(&state, &sess, &request(&[("ids", json!("9011"))])).await;
    let got = result["multiselect"].as_str().unwrap();

    // Flat list, title order: All feeds (include_all_feeds), then the seed and
    // fixture feeds together — Ars, Feed A1, Feed A2, Feed D, Feed M, Hacker
    // News, MIT, The Register, TopZ, UnCat 1, UnCat 2 — no "Uncategorized",
    // no em-space indentation, no categories.
    let want = [
        format!(
            "<select multiple={tab}rue\" id=\"feed_id\" name=\"feed_id\" {attrs}>",
            tab = '\t',
            attrs = ATTRS
        ),
        "<option  value=\"0\">All feeds</option>".to_string(),
        "<option  value='3'>Ars Technica</option>".to_string(),
        "<option  value='9011'>Feed A1</option>".to_string(),
        "<option  value='9010'>Feed A2</option>".to_string(),
        "<option  value='9013'>Feed D</option>".to_string(),
        "<option  value='9012'>Feed M</option>".to_string(),
        "<option  value='1'>Hacker News</option>".to_string(),
        "<option  value='4'>MIT Tech Review</option>".to_string(),
        "<option  value='2'>The Register</option>".to_string(),
        "<option  value='9016'>TopZ</option>".to_string(),
        "<option  value='9014'>UnCat 1</option>".to_string(),
        "<option  value='9015'>UnCat 2</option>".to_string(),
        "</select>".to_string(),
    ]
    .join("");
    // The selected feed (9011) gets selected="1".
    let want = want.replace("<option  value='9011'>", "<option selected=\"1\" value='9011'>");
    assert_eq!(got, &want, "flat multi-select mismatch");
        Ok(())
    })).await.unwrap();
}
