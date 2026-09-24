//! DB test for the `Pref_Filters` i18n (M10.3) — the translated filter
//! strings under a non-English locale (`de_DE`).
//!
//! Covers the two distinct code paths: the rule name (`get_rule_name` →
//! `__` + `t_sprintf`) and the filter-details title summary
//! (`filter_details` → `t_nsprintf` + `__`).
//!
//! Rust-only: the frozen PHP suite has no filter-configuration locale test.

mod common;

use serde_json::{json, Map, Value};
use ttrss::core::config::Config;
use ttrss::core::db::Db;
use ttrss::core::phpser::Value as PValue;
use ttrss::core::session::Session;
use ttrss::session::Sess;
use ttrss::AppState;
use test_db::template::with_test_db;

/// A test config whose `docroot` points at the frozen `tt-rss` tree so the
/// real gettext catalogs load (the relative `for_test` docroot is not
/// resolvable from the crate dir).
fn test_config() -> Config {
    let mut cfg = Config::for_test();
    cfg.plugins = "auth_internal".into();
    cfg.docroot = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tt-rss");
    cfg
}

/// Fresh fixtures + a session carrying `uid = 1` (owner of the fixture rows).
async fn setup(url: &str) -> (AppState, Sess) {
    let db: Db = common::make_db_from_url(url).await.expect("test db");
    let state = AppState::new(test_config(), db);
    let mut data = Session::new();
    data.set("uid", PValue::Int(1));
    let sess = Sess::new("test-sid".to_string());
    sess.replace("test-sid".to_string(), data);
    (state, sess)
}

/// `$_REQUEST` payload as the handler's `extra` map.
fn request(pairs: &[(&str, Value)]) -> Map<String, Value> {
    Map::from_iter(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

/// Build the per-request `I18nCtx` for a locale (`None` = English).
fn ctx(state: &AppState, locale: Option<&str>) -> ttrss::i18n::I18nCtx {
    ttrss::i18n::I18nCtx {
        locale: locale.map(|s| s.to_string()),
        cache: state.i18n.clone(),
    }
}

/// `print_rule_name` under `locale`: a Title rule across All feeds
/// (`feed_id = ["0"]`).
async fn rule_name(state: &AppState, sess: &Sess, locale: Option<&str>) -> String {
    let rule = json!({ "reg_exp": "rust", "filter_type": 1, "feed_id": ["0"] });
    let rule_str = json!(rule.to_string());
    ttrss::i18n::I18N
        .scope(ctx(state, locale), async {
            ttrss::filters::print_rule_name(state, sess, &request(&[("rule", rule_str)])).await
        })
        .await
}

/// The `name` (title summary) of the filter `fid` in `get_filter_tree`, under
/// `locale`.
async fn filter_name(
    state: &AppState,
    sess: &Sess,
    fid: i32,
    locale: Option<&str>,
) -> String {
    ttrss::i18n::I18N
        .scope(ctx(state, locale), async {
            let tree = ttrss::filters::get_filter_tree(state, sess).await;
            let items = &tree["items"][0]["items"];
            items
                .as_array()
                .expect("tree items")
                .iter()
                .find(|it| it["bare_id"].as_i64() == Some(fid as i64))
                .expect("filter node")
                ["name"]
                .as_str()
                .expect("title summary")
                .to_string()
        })
        .await
}

/// The rule name localizes: `Title` → `Titel`, `All feeds` → `Alle Feeds`,
/// and `"%s on %s in %s %s"` → `"%s innerhalb %s von %s %s"`.
#[tokio::test]
async fn test_filter_rule_name_de_de() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;

        let en = rule_name(&state, &sess, None).await;
        assert!(
            en.contains("rust on <span class='field'>Title</span> in <span class='feed'>All feeds</span>"),
            "English rule name: {en}"
        );

        let de = rule_name(&state, &sess, Some("de_DE")).await;
        assert!(
            de.contains("rust innerhalb <span class='field'>Titel</span> von <span class='feed'>Alle Feeds</span>"),
            "German rule name: {de}"
        );
        Ok(())
    }))
    .await
    .unwrap();
}

/// The filter-details title summary localizes: the rule count
/// (`"%s (%d rule)"` → `"%s (%d Regel)"`) and `matches any rule` →
/// `erfüllt beliebige Regel`.
#[tokio::test]
async fn test_filter_details_de_de() {
    with_test_db(|url| Box::pin(async move {
        let (state, sess) = setup(&url).await;

        // A filter (match_any_rule) with one Title rule across All feeds.
        let fid: i32 = sqlx::query_scalar(
            "INSERT INTO ttrss_filters2 (owner_uid, title, enabled, match_any_rule, inverse) \
             VALUES (1, 'test', true, true, false) RETURNING id",
        )
        .fetch_one(state.db.pool())
        .await
        .expect("insert filter");
        sqlx::query(
            "INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type, feed_id) \
             VALUES ($1, 'foo', 1, NULL)",
        )
        .bind(fid)
        .execute(state.db.pool())
        .await
        .expect("insert rule");

        assert_eq!(
            filter_name(&state, &sess, fid, None).await,
            "test (1 rule), matches any rule"
        );
        assert_eq!(
            filter_name(&state, &sess, fid, Some("de_DE")).await,
            "test (1 Regel), erfüllt beliebige Regel"
        );
        Ok(())
    }))
    .await
    .unwrap();
}
