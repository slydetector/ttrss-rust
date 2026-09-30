//! `Pref_Filters` backend read handlers (the filters UI in
//! `backend.php?op=Pref_Filters`).

use minijinja::Environment;
use serde_json::{json, Map, Value};
use sqlx::Row;
use sqlx::AssertSqlSafe;
use std::collections::HashMap;

use crate::prefs;
use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util;
use crate::util::htmlspecialchars;
use crate::i18n::{__, t_sprintf, t_nsprintf};
use crate::AppState;

// `Pref_Filters` action / type constants.
const ACTION_TAG: i64 = 4;
const ACTION_SCORE: i64 = 6;
const ACTION_LABEL: i64 = 7;
const ACTION_PLUGIN: i64 = 9;
const ACTION_REMOVE_TAG: i64 = 10;
const PARAM_ACTIONS: [i64; 5] = [
    ACTION_TAG,
    ACTION_SCORE,
    ACTION_LABEL,
    ACTION_PLUGIN,
    ACTION_REMOVE_TAG,
];
const MAX_ACTIONS_TO_DISPLAY: usize = 3;

/// `Pref_Filters::$filter_actions` (action_id → description).
fn action_description(action_id: i64) -> Option<String> {
    Some(match action_id {
        1 => __("Delete article"),
        2 => __("Mark as read"),
        3 => __("Set starred"),
        4 => __("Assign tags"),
        5 => __("Publish article"),
        6 => __("Modify score"),
        7 => __("Assign label"),
        8 => __("Stop / Do nothing"),
        9 => __("Invoke plugin"),
        10 => __("Ignore tags"),
        _ => return None,
    })
}

/// `Pref_Filters::$filter_types` (filter_type → description).
fn filter_type_description(t: i64) -> Option<String> {
    Some(match t {
        1 => __("Title"),
        2 => __("Content"),
        3 => __("Title or Content"),
        4 => __("Link"),
        6 => __("Author"),
        7 => __("Article Tags"),
        _ => return None,
    })
}

/// `Pref_Filters::_get_action_name`.
fn action_name(action_id: i64, action_param: &str) -> String {
    let base = match action_description(action_id) {
        Some(d) => d,
        None => t_sprintf("Unknown action: %d", &[&action_id.to_string()]),
    };
    // (ACTION_PLUGIN is skipped — no plugin filter actions in the port.)
    if PARAM_ACTIONS.contains(&action_id) {
        format!("{base}: {action_param}")
    } else {
        base
    }
}

/// `Pref_Filters::_get_rule_name` — the `… on TYPE in FEED (inverse)` name for a
/// rule's `feed_id` list, `filter_type`, `reg_exp`, and `inverse` key presence.
/// `html_format` wraps the parts in the `filterRule`/`field`/`feed` spans.
async fn get_rule_name(
    pool: &sqlx::PgPool,
    uid: i64,
    rule: &Map<String, Value>,
    html_format: bool,
) -> String {
    let feeds = rule.get("feed_id").cloned().unwrap_or(Value::Null);
    let feeds_vec: Vec<Value> = match feeds {
        Value::Array(v) => v,
        other => vec![other],
    };
    let mut feeds_fmt: Vec<String> = Vec::new();
    for feed_id in &feeds_vec {
        let s = util::scalar_str(feed_id).unwrap_or_default();
        if let Some(cat) = s.strip_prefix("CAT:") {
            feeds_fmt.push(crate::feeds::category_title(pool, util::php_int_cast(cat), uid).await);
        } else {
            let fid = util::php_int_cast(&s);
            feeds_fmt.push(if fid != 0 {
                crate::feeds::feed_title(pool, fid, uid).await
            } else {
                __("All feeds")
            });
        }
    }
    let feed = feeds_fmt.join(", ");
    let filter_type_desc = filter_type_description(util::clean_int(rule.get("filter_type")))
        .unwrap_or_else(|| "unknown filter type".to_string());
    let inverse = rule.get("inverse").is_some();
    let inverse_text = if inverse { __("(inverse)")} else { String::new() };
    if html_format {
        let reg_exp = rule
            .get("reg_exp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let inner = t_sprintf(
            "%s on %s in %s %s",
            &[
                &htmlspecialchars(&reg_exp),
                &format!(
                    "<span class='field'>{}</span>",
                    htmlspecialchars(&filter_type_desc)
                ),
                &format!("<span class='feed'>{}</span>", htmlspecialchars(&feed)),
                &inverse_text,
            ],
        );
        format!(
            "<span class='filterRule {}'>{inner}</span>",
            if inverse { "inverse" } else { "" }
        )
    } else {
        let reg_exp = rule.get("reg_exp").and_then(Value::as_str).unwrap_or("");
        t_sprintf(
            "%s on %s in %s %s",
            &[reg_exp, &filter_type_desc, &feed, &inverse_text],
        )
    }
}
/// `Pref_Filters::_get_details` — `{title_summary, actions_summary}` for a filter.
async fn filter_details(pool: &sqlx::PgPool, id: i32) -> (String, String) {
    let (title, match_any, inverse, num_rules): (String, bool, bool, i64) = sqlx::query_as(
        "SELECT f.title, f.match_any_rule, f.inverse, COUNT(DISTINCT r.id) \
             FROM ttrss_filters2 AS f \
             LEFT JOIN ttrss_filters2_rules AS r ON r.filter_id = f.id \
             WHERE f.id = $1 GROUP BY f.title, f.match_any_rule, f.inverse",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|_| (__("[No caption]"), false, false, 0));

    let title = if title.is_empty() {
        __("[No caption]")
    } else {
        title
    };

    // title_summary: "%s (N rule[rules])" + optional "matches any rule" / "inverse".
    let mut parts: Vec<String> = Vec::new();
    parts.push(t_nsprintf(
        "%s (%d rule)",
        "%s (%d rules)",
        num_rules,
        &[&title, &num_rules.to_string()],
    ));
    if match_any {
        parts.push(__("matches any rule"));
    }
    if inverse {
        parts.push(__("inverse"));
    }
    let title_summary = parts.join(", ");

    // actions_summary: per-action <li> items, score actions collapsed to a
    // cumulative "Modify score: N" (unshifted first), capped at MAX with "(+N)".
    let actions = sqlx::query(
        "SELECT action_id, action_param FROM ttrss_filters2_actions \
         WHERE filter_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut summary: Vec<String> = Vec::new();
    let mut cumulative_score: i64 = 0;
    for a in &actions {
        let action_id: i64 = a.get::<i32, _>(0) as i64;
        let action_param: String = a.get(1);
        if action_id == ACTION_SCORE {
            cumulative_score += action_param.parse::<i64>().unwrap_or(0);
            continue;
        }
        summary.push(format!(
            "<li>{}</li>",
            htmlspecialchars(&action_name(action_id, &action_param))
        ));
    }
    if cumulative_score != 0 {
        summary.insert(
            0,
            format!(
                "<li>{}</li>",
                htmlspecialchars(&action_name(ACTION_SCORE, &cumulative_score.to_string()))
            ),
        );
    }
    if summary.len() > MAX_ACTIONS_TO_DISPLAY {
        let not_shown = summary.len() - MAX_ACTIONS_TO_DISPLAY;
        summary.truncate(MAX_ACTIONS_TO_DISPLAY);
        summary.push(format!(
            "<li class=\"text-muted\"><em>{}</em></li>",
            t_nsprintf(
                "(+%d action)",
                "(+%d actions)",
                not_shown as i64,
                &[&not_shown.to_string()],
            )
        ));
    }

    (title_summary, summary.join(""))
}

/// `Pref_Filters::_get_rules_list` — the `<li class='...'>...on ...in ...</li>`
/// string for a filter's rules (ordered by reg_exp, id).
async fn rules_list(pool: &sqlx::PgPool, filter_id: i32, uid: i64) -> String {
    let rules = sqlx::query(
        "SELECT reg_exp, filter_type, feed_id, cat_filter, cat_id, match_on, inverse \
         FROM ttrss_filters2_rules WHERE filter_id = $1 ORDER BY reg_exp, id",
    )
    .bind(filter_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut rv = String::new();
    for r in &rules {
        let reg_exp: String = r.get::<String, _>(0);
        let filter_type: i64 = r.get::<i32, _>(1) as i64;
        let feed_id: Option<i64> = r.get::<Option<i32>, _>(2).map(|v| v as i64);
        let cat_filter: bool = r.get::<bool, _>(3);
        let cat_id: Option<i64> = r.get::<Option<i32>, _>(4).map(|v| v as i64);
        let match_on: Option<String> = r.get::<Option<String>, _>(5);
        let inverse: bool = r.get::<bool, _>(6);

        let where_str =
            rule_where(pool, uid, feed_id, cat_filter, cat_id, match_on.as_deref()).await;

        let inverse_class = if inverse { "inverse" } else { "" };
        let desc = filter_type_description(filter_type).unwrap_or_default();
        rv.push_str(&format!(
            "<li class='{inverse_class}'>{}</li>",
            t_sprintf(
                "%s on %s in %s %s",
                &[
                    &htmlspecialchars(&reg_exp),
                    &desc,
                    &htmlspecialchars(&where_str),
                    &if inverse {
                        __("(inverse)")
                    } else {
                        String::new()
                    },
                ]
            )
        ));
    }
    rv
}

/// The rule's `in ...` clause: the `match_on` feed list, else the single
/// category / feed, else "All feeds".
async fn rule_where(
    pool: &sqlx::PgPool,
    uid: i64,
    feed_id: Option<i64>,
    cat_filter: bool,
    cat_id: Option<i64>,
    match_on: Option<&str>,
) -> String {
    if let Some(mo) = match_on {
        let mo = mo.trim();
        if !mo.is_empty()
            && let Ok(referenced) = serde_json::from_str::<Vec<Value>>(mo) {
                let mut parts: Vec<String> = Vec::new();
                for f in &referenced {
                    if let Some(fid) = f.as_str() {
                        if let Some(rest) = fid.strip_prefix("CAT:") {
                            parts.push(
                                crate::feeds::category_title(pool, rest.parse().unwrap_or(0), uid)
                                    .await,
                            );
                        } else if fid.is_empty() {
                            parts.push(__("All feeds"));
                        } else {
                            parts.push(
                                crate::feeds::feed_title(pool, fid.parse().unwrap_or(0), uid).await,
                            );
                        }
                    }
                }
                return parts.join(", ");
            }
    }
    if cat_filter {
        crate::feeds::category_title(pool, cat_id.unwrap_or(0), uid).await
    } else if let Some(fid) = feed_id {
        crate::feeds::feed_title(pool, fid, uid).await
    } else {
        __("All feeds")
    }
}

/// `Pref_Filters::getfiltertree`.
pub async fn get_filter_tree(state: &AppState, sess: &Sess) -> Value {
    let pool = state.db.pool();
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let filter_search = data
        .get_str("prefs_filter_search")
        .unwrap_or("")
        .to_string();

    let filters = sqlx::query(
        "SELECT id, title, match_any_rule, inverse, enabled, \n         to_char(last_triggered, 'YYYY-MM-DD HH24:MI:SS') AS last_triggered \n         FROM ttrss_filters2 WHERE owner_uid = $1 ORDER BY order_id, title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut items: Vec<Value> = Vec::new();
    for f in &filters {
        let id: i32 = f.get(0);
        let title: String = f.get(1);
        let enabled: bool = f.get(4);
        let last_triggered: Option<String> = f.get(5);

        if !filter_search.is_empty() {
            let in_title = title.to_lowercase().contains(&filter_search.to_lowercase());
            let in_rule = rule_match(pool, id, &filter_search).await;
            if !in_title && !in_rule {
                continue;
            }
        }

        let bare_name = if title.is_empty() {
            __("[No caption]")
        } else {
            title
        };
        let (title_summary, actions_summary) = filter_details(pool, id).await;
        let rules = rules_list(pool, id, uid).await;

        let last_triggered_json = match last_triggered.as_deref().filter(|t| !t.is_empty()) {
            Some(ts) => {
                let rows = prefs::load_rows(&state.db, uid, profile).await;
                let tz = TzCtx {
                    user_tz: prefs::resolve(prefs::USER_TIMEZONE, &rows, profile)
                        .as_str()
                        .to_string(),
                    client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
                    short_date_format: prefs::resolve(prefs::SHORT_DATE_FORMAT, &rows, profile)
                        .as_str()
                        .to_string(),
                    long_date_format: prefs::resolve(prefs::LONG_DATE_FORMAT, &rows, profile)
                        .as_str()
                        .to_string(),
                };
                Value::String(make_local_datetime(Some(ts), false, &tz))
            }
            None => Value::Null,
        };

        items.push(json!({
            "id": format!("FILTER:{}", id),
            "bare_id": id,
            "bare_name": bare_name,
            "name": title_summary,
            "param": actions_summary,
            "checkbox": false,
            "last_triggered": last_triggered_json,
            "enabled": enabled,
            "rules": rules,
        }));
    }

    json!({
        "identifier": "id",
        "label": "name",
        "items": [{
            "id": "root",
            "name": __("Filters"),
            "enabled": true,
            "items": items,
        }],
    })
}

/// `LOWER(reg_exp) LIKE LOWER('%search%')` for a filter.
async fn rule_match(pool: &sqlx::PgPool, filter_id: i32, search: &str) -> bool {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2_rules \n         WHERE filter_id = $1 AND LOWER(reg_exp) LIKE LOWER($2)",
    )
    .bind(filter_id)
    .bind(format!("%{search}%"))
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    n > 0
}

/// `Pref_Filters::edit` — the edit-dialog prefill: the filter row (or defaults
/// for a new filter, `id = 0`), its rule/action lists, the `filter_types` /
/// `action_types` maps, and the owner's labels. Returns `None` when the id is
/// nonzero but not found (the PHP prints nothing).
pub async fn edit(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Option<Value> {
    use sqlx::Row as _;
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let filter_id = util::clean_int(extra.get("id")) as i32;

    let row = sqlx::query(
        "SELECT id, enabled, match_any_rule, inverse, title FROM ttrss_filters2 WHERE id = $1 AND owner_uid = $2",
    )
    .bind(filter_id)
    .bind(uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if !(filter_id == 0 || row.is_some()) {
        return None;
    }

    let (enabled, match_any, inverse, title) = row
        .as_ref()
        .map(|r| {
            (
                r.get::<bool, _>(1),
                r.get::<bool, _>(2),
                r.get::<bool, _>(3),
                r.get::<String, _>(4),
            )
        })
        .unwrap_or((true, false, false, String::new()));

    let mut filter_types = Map::new();
    for id in [1, 2, 3, 4, 6, 7] {
        if let Some(desc) = filter_type_description(id) {
            filter_types.insert(id.to_string(), Value::String(desc));
        }
    }
    let mut action_types = Map::new();
    for id in 1..=10 {
        if let Some(desc) = action_description(id) {
            action_types.insert(id.to_string(), Value::String(desc));
        }
    }

    let mut rules: Vec<Value> = Vec::new();
    let mut actions: Vec<Value> = Vec::new();
    if filter_id > 0 {
        // Rules (ORDER BY reg_exp, id — matching `_get_rules_list`).
        let rule_rows = sqlx::query(
            "SELECT reg_exp, filter_type, feed_id, cat_id, match_on, cat_filter, inverse \
             FROM ttrss_filters2_rules WHERE filter_id = $1 ORDER BY reg_exp, id",
        )
        .bind(filter_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for r in &rule_rows {
            let reg_exp: String = r.get::<String, _>(0);
            let filter_type: i32 = r.get::<i32, _>(1);
            let feed_id: Option<i32> = r.get::<Option<i32>, _>(2);
            let cat_id: Option<i32> = r.get::<Option<i32>, _>(3);
            let match_on: Option<String> = r.get::<Option<String>, _>(4);
            let cat_filter: bool = r.get::<bool, _>(5);
            let inverse: bool = r.get::<bool, _>(6);

            let mut rule = Map::new();
            rule.insert("reg_exp".into(), Value::String(reg_exp));
            rule.insert("filter_type".into(), Value::from(filter_type));
            // feed_id: the `match_on` JSON array, else the single cat/feed id
            // as a one-element string array (so `in_array` treats it as a string).
            match match_on.as_deref().filter(|m| !m.is_empty()) {
                Some(mo) => {
                    rule.insert(
                        "feed_id".into(),
                        serde_json::from_str(mo)
                            .unwrap_or(Value::Array(vec![Value::String(String::new())])),
                    );
                }
                None => {
                    let fid = if cat_filter {
                        format!("CAT:{}", cat_id.unwrap_or(0))
                    } else {
                        feed_id.unwrap_or(0).to_string()
                    };
                    rule.insert("feed_id".into(), Value::Array(vec![Value::String(fid)]));
                }
            }
            // `inverse` is omitted when false (the checkbox is unset when unchecked).
            if inverse {
                rule.insert("inverse".into(), Value::Bool(true));
            }
            let name = get_rule_name(pool, uid, &rule, true).await;
            rule.insert("name".into(), Value::String(name));
            rules.push(Value::Object(rule));
        }
        // Actions (ORDER BY id).
        let action_rows = sqlx::query(
            "SELECT action_id, action_param FROM ttrss_filters2_actions WHERE filter_id = $1 ORDER BY id",
        )
        .bind(filter_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for a in &action_rows {
            let action_id: i32 = a.get::<i32, _>(0);
            let action_param: String = a.get::<String, _>(1);
            let mut action = Map::new();
            action.insert("action_id".into(), Value::from(action_id));
            action.insert("action_param".into(), Value::String(action_param.clone()));
            action.insert(
                "action_param_label".into(),
                Value::String(action_param.clone()),
            );
            action.insert(
                "name".into(),
                Value::String(action_name(action_id as i64, &action_param)),
            );
            actions.push(Value::Object(action));
        }
    }

    let labels = crate::labels::get_all(pool, uid).await;
    Some(json!({
        "id": filter_id,
        "enabled": enabled,
        "match_any_rule": match_any,
        "inverse": inverse,
        "title": title,
        "rules": rules,
        "actions": actions,
        "filter_types": filter_types,
        "action_types": action_types,
        "plugin_actions": Value::Object(Map::new()),
        "labels": labels,
    }))
}

/// `Pref_Filters::printRuleName` — the rule name for a single rule JSON (the
/// `rule` param is a JSON string; a non-object decodes to `invalid rule JSON`).
pub async fn print_rule_name(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let Some(raw) = extra.get("rule") else {
        return "malformed rule JSON".to_string();
    };
    let val = match raw {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => return "malformed rule JSON".to_string(),
        },
        other => other.clone(),
    };
    match val.as_object() {
        Some(rule) => get_rule_name(pool, uid, rule, true).await,
        None => "invalid rule JSON".to_string(),
    }
}

/// `Pref_Filters::printActionName` — the action name for a single action JSON.
pub async fn print_action_name(
    _state: &AppState,
    _sess: &Sess,
    extra: &Map<String, Value>,
) -> String {
    let Some(raw) = extra.get("action") else {
        return "malformed action JSON".to_string();
    };
    let val = match raw {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => return "malformed action JSON".to_string(),
        },
        other => other.clone(),
    };
    match val.as_object() {
        Some(action) => {
            let action_id = util::clean_int(action.get("action_id"));
            let action_param = action
                .get("action_param")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            action_name(action_id, &action_param)
        }
        None => "invalid action JSON".to_string(),
    }
}

/// `Pref_Filters::_feed_multi_select` — a `<select multiple>` of the owner's
/// feeds, grouped under the category tree when `enable_cats` is set (otherwise a
/// flat feed list). `default_ids` pre-selects feeds (by id string) and
/// categories (by `CAT:<id>`), plus the "All feeds" (`"0"`) and "Uncategorized"
/// (`"CAT:0"`) options.
///
/// Ported iteratively (explicit work stack) per the AGENTS.md rule: the PHP
/// recurses down the category tree, emitting each category, then its whole
/// subtree, then the category's own feeds.
async fn feed_multi_select(
    pool: &sqlx::PgPool,
    uid: i64,
    id: &str,
    default_ids: &[String],
    attributes: &str,
    include_all_feeds: bool,
    enable_cats: bool,
) -> String {
    let mut rv = String::new();

    // Open the `<select>` (top level only — the recursive calls nest inside it).
    rv.push_str(&format!(
        "<select multiple={tab}rue\" id=\"{}\" name=\"{}\" {}>",
        id,
        id,
        attributes,
        tab = '\t'
    ));
    if include_all_feeds {
        let sel = if default_ids.contains(&"0".to_string()) {
            "selected=\"1\""
        } else {
            ""
        };
        rv.push_str(&format!("<option {} value=\"0\">{}</option>", sel, __("All feeds")));
    }

    if enable_cats {
        // Categories grouped by parent (None = top-level) and feeds grouped by
        // cat_id (None = uncategorized), each title-ordered (the PHP does a
        // per-level `ORDER BY title`).
        let cats: Vec<(i32, String, Option<i32>)> = sqlx::query_as(
            "SELECT id, title, parent_cat FROM ttrss_feed_categories WHERE owner_uid = $1",
        )
        .bind(uid)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        let mut by_parent: HashMap<Option<i32>, Vec<(i32, String)>> = HashMap::new();
        for (cid, title, parent) in &cats {
            by_parent
                .entry(*parent)
                .or_default()
                .push((*cid, title.clone()));
        }
        for v in by_parent.values_mut() {
            v.sort_by(|a, b| a.1.cmp(&b.1));
        }

        let feeds: Vec<(i32, String, Option<i32>)> =
            sqlx::query_as("SELECT id, title, cat_id FROM ttrss_feeds WHERE owner_uid = $1")
                .bind(uid)
                .fetch_all(pool)
                .await
                .unwrap_or_default();
        let mut feeds_by_cat: HashMap<Option<i32>, Vec<(i32, String)>> = HashMap::new();
        for (fid, title, cat) in &feeds {
            feeds_by_cat
                .entry(*cat)
                .or_default()
                .push((*fid, title.clone()));
        }
        for v in feeds_by_cat.values_mut() {
            v.sort_by(|a, b| a.1.cmp(&b.1));
        }

        let cat_title = |cid: i32| -> String {
            cats.iter()
                .find(|(id, _, _)| *id == cid)
                .map(|(_, t, _)| t.clone())
                .unwrap_or_default()
        };
        // Em-space (U+2003) indentation by `level` (the PHP prepends one per level).
        let indent_str = |title: &str, level: i32| -> String {
            let mut s = String::with_capacity(title.len() + level as usize);
            for _ in 0..level {
                s.push('\u{2003}');
            }
            s.push_str(title);
            s
        };
        // Work-stack DFS. `ProcessCat` emits the category, schedules the feeds
        // (after the children), then the children in reverse so the first child
        // is processed first. `EmitFeeds` emits one category's feeds.
        enum Task {
            ProcessCat(i32, i32),
            EmitFeeds(i32, i32),
        }
        let mut stack: Vec<Task> = by_parent
            .get(&None)
            .map(|top| {
                top.iter()
                    .rev()
                    .map(|(cid, _)| Task::ProcessCat(*cid, 0))
                    .collect()
            })
            .unwrap_or_default();

        while let Some(task) = stack.pop() {
            match task {
                Task::ProcessCat(cid, level) => {
                    let title = cat_title(cid);
                    let sel = if default_ids.contains(&format!("CAT:{}", cid)) {
                        "selected=\"1\""
                    } else {
                        ""
                    };
                    let indented = indent_str(&title, level);
                    rv.push_str(&format!(
                        "<option {} value='CAT:{}'>{}</option>",
                        sel,
                        cid,
                        htmlspecialchars(&indented)
                    ));
                    stack.push(Task::EmitFeeds(cid, level + 1));
                    if let Some(children) = by_parent.get(&Some(cid)) {
                        for (child, _) in children.iter().rev() {
                            stack.push(Task::ProcessCat(*child, level + 1));
                        }
                    }
                }
                Task::EmitFeeds(cid, indent) => {
                    if let Some(feeds) = feeds_by_cat.get(&Some(cid)) {
                        for (fid, ftitle) in feeds {
                            let sel = if default_ids.contains(&fid.to_string()) {
                                "selected=\"1\""
                            } else {
                                ""
                            };
                            let indented = indent_str(ftitle, indent);
                            rv.push_str(&format!(
                                "<option {} value='{}'>{}</option>",
                                sel,
                                fid,
                                htmlspecialchars(&indented)
                            ));
                        }
                    }
                }
            }
        }

        // "Uncategorized" category + uncategorized feeds (top level only).
        let sel = if default_ids.contains(&"CAT:0".to_string()) {
            "selected=\"1\""
        } else {
            ""
        };
        rv.push_str(&format!(
            "<option {} value='CAT:0'>Uncategorized</option>",
            sel
        ));
        if let Some(feeds) = feeds_by_cat.get(&None) {
            for (fid, ftitle) in feeds {
                let sel = if default_ids.contains(&fid.to_string()) {
                    "selected=\"1\""
                } else {
                    ""
                };
                let indented = indent_str(ftitle, 1);
                rv.push_str(&format!(
                    "<option {} value='{}'>{}</option>",
                    sel,
                    fid,
                    htmlspecialchars(&indented)
                ));
            }
        }
    } else {
        // No categories: a flat, title-ordered feed list (no indentation).
        let feeds: Vec<(i32, String)> =
            sqlx::query_as("SELECT id, title FROM ttrss_feeds WHERE owner_uid = $1 ORDER BY title")
                .bind(uid)
                .fetch_all(pool)
                .await
                .unwrap_or_default();
        for (fid, ftitle) in feeds {
            let sel = if default_ids.contains(&fid.to_string()) {
                "selected=\"1\""
            } else {
                ""
            };
            rv.push_str(&format!(
                "<option {} value='{}'>{}</option>",
                sel,
                fid,
                htmlspecialchars(&ftitle)
            ));
        }
    }

    rv.push_str("</select>");
    rv
}

/// `Pref_Filters::editrule` — the feed multi-select for a rule. `ids` is a
/// comma-separated list of feed ids and `CAT:<id>` category strings (cleaned, so
/// the category strings are preserved — not run through `_param_to_int_array`).
/// Returns `{"multiselect": <select html>}`.
pub async fn editrule(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let pref_rows = prefs::load_rows(&state.db, uid, None).await;
    let enable_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &pref_rows, profile).as_bool();

    let raw = extra.get("ids").and_then(Value::as_str).unwrap_or("");
    let ids = util::clean_str(raw);
    let feed_ids: Vec<String> = ids.split(',').map(|s| s.to_string()).collect();

    let multiselect = feed_multi_select(
        pool,
        uid,
        "feed_id",
        &feed_ids,
        r#"required="1" style="width : 100%; height : 300px" dojoType="fox.form.ValidationMultiSelect""#,
        true,
        enable_cats,
    )
    .await;
    json!({ "multiselect": multiselect })
}

/// A posted `rule[]` JSON — kept both raw (for `_get_rule_name`) and as the
/// typed eval representation.
struct PostedRule {
    raw: Value,
    eval_rule: crate::core::filters::EvalRule,
    /// The `feed_id` elements as strings (PHP `"$feed_id"` casts).
    feed_ids: Vec<String>,
}

impl PostedRule {
    fn from_json(v: &Value) -> Self {
        let reg_exp = v
            .get("reg_exp")
            .and_then(util::scalar_str)
            .unwrap_or_default();
        let filter_type = util::clean_int(v.get("filter_type"));
        // `$rule['inverse'] ??= false;` — truthy-checked by the engine.
        let inverse = v.get("inverse").map(json_truthy).unwrap_or(false);
        let feeds = v.get("feed_id").cloned().unwrap_or(Value::Null);
        let feeds_vec = match &feeds {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let feed_ids: Vec<String> = feeds_vec.iter().filter_map(util::scalar_str).collect();
        PostedRule {
            raw: v.clone(),
            eval_rule: crate::core::filters::EvalRule {
                reg_exp,
                rule_type: crate::core::filters::RuleType::from_id(filter_type as i32),
                inverse,
            },
            feed_ids,
        }
    }
}

/// `Pref_Filters::testFilterDo` — the "Test Filter" dialog: evaluates the
/// rules the dialog currently holds (posted `rule[]` JSON) as a single fake
/// filter over the user's recent entries and returns the pre-filtering count
/// (the page size) plus the matching items (highlighted title / preview and
/// plain rule names). The filter's actions are the placeholder
/// `'dummy-action'` — only the match itself matters.
///
/// `Err(())` is the PHP `max_execution_time` fatal: a matched rule whose
/// highlight word (the match, `strip_tags`-ed and cut to 200 chars) is empty
/// — e.g. a filter that matched via `inverse` (the rule matched nothing
/// textually) or a regex matching the empty string — makes PHP's
/// `Sanitizer::highlight_words` loop forever, and the request 500s with an
/// empty body (verified against the oracle: ~39 s → fatal).
#[allow(clippy::result_unit_err)] // the only error is the fatal itself
pub async fn test_filter_do(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<Value, ()> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);

    let offset = util::clean_int(extra.get("offset"));
    let limit = util::clean_int(extra.get("limit"));
    let match_any_rule = util::checkbox(extra, "match_any_rule");
    let inverse = util::checkbox(extra, "inverse");

    // `$_REQUEST['rule']`: an array of JSON strings; invalid JSON and
    // non-object/array values are skipped (PHP try/catch + `is_array`).
    let mut posted: Vec<PostedRule> = Vec::new();
    let rule_param = extra.get("rule").cloned().unwrap_or(Value::Null);
    let rule_items = match &rule_param {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    for it in rule_items {
        let Some(s) = it.as_str() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(s) else {
            continue;
        };
        if !matches!(v, Value::Object(_) | Value::Array(_)) {
            continue;
        }
        posted.push(PostedRule::from_json(&v));
    }

    // Scope parts: per rule, the feed/category restrictions (OR-joined
    // within a rule; the rules join by `match_any_rule`).
    let mut scope_qparts: Vec<String> = Vec::new();
    for pr in &posted {
        let mut parts: Vec<String> = Vec::new();
        for fid in &pr.feed_ids {
            if let Some(rest) = fid.strip_prefix("CAT:") {
                let cat_id = util::php_int_cast(rest);
                parts.push(if cat_id > 0 {
                    format!("cat_id = {cat_id}")
                } else {
                    "cat_id IS NULL".to_string()
                });
            } else if util::is_numeric(fid) && util::php_int_cast(fid) > 0 {
                parts.push(format!("feed_id = {}", util::php_int_cast(fid)));
            }
        }
        if !parts.is_empty() {
            scope_qparts.push(format!("({})", parts.join(" OR ")));
        }
    }

    let mut sql = format!(
        "SELECT e.title, e.content, e.date_entered, e.link, e.author, ue.tag_cache, \
         f.id AS feed_id, f.title AS feed_title, fc.id AS cat_id \
         FROM ttrss_entries e \
         JOIN ttrss_user_entries ue ON ue.ref_id = e.id \
         LEFT JOIN ttrss_feeds f ON f.id = ue.feed_id \
         LEFT JOIN ttrss_feed_categories fc ON fc.id = f.cat_id \
         WHERE ue.owner_uid = {uid}"
    );
    if !scope_qparts.is_empty() {
        sql.push_str(&format!(
            " AND ({})",
            scope_qparts.join(if match_any_rule { " OR " } else { " AND " })
        ));
    }
    sql.push_str(&format!(
        " ORDER BY e.date_entered DESC LIMIT {limit} OFFSET {offset}"
    ));

    let rows = match sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await {
        Ok(r) => r,
        // A SQL failure (e.g. a negative LIMIT) degrades to an empty result
        // (PHP would 500; the SPA only sends non-negative values).
        Err(_) => return Ok(json!({ "pre_filtering_count": 0, "items": [] })),
    };

    let empty_map = Map::new();
    let mut items: Vec<Value> = Vec::new();
    for r in &rows {
        let title: String = r.get("title");
        let content: String = r.get("content");
        let date_entered: Option<chrono::NaiveDateTime> = r.get("date_entered");
        let link: String = r.get("link");
        let author: Option<String> = r.get("author");
        let tag_cache: String = r.get("tag_cache");
        let feed_id: Option<i32> = r.get("feed_id");
        let feed_title: Option<String> = r.get("feed_title");
        let cat_id: Option<i32> = r.get("cat_id");

        let author = author.unwrap_or_default();
        // `explode(",", $entry['tag_cache'])`
        let tags: Vec<String> = tag_cache.split(',').map(str::to_string).collect();

        // The rules applicable to this entry's feed/category: `CAT:0` +
        // uncategorized, `CAT:<cat>` (incl. `CAT:` + null), the feed id
        // (strict int compare), or `0` (all feeds).
        let cat_str = cat_id
            .map(|c| format!("CAT:{c}"))
            .unwrap_or_else(|| "CAT:".to_string());
        let mut entry_rules: Vec<&PostedRule> = Vec::new();
        for pr in &posted {
            for fid in &pr.feed_ids {
                let in_scope = fid == cat_str.as_str()
                    || (fid == "CAT:0" && cat_id.is_none())
                    || feed_id
                        .map(|f| util::php_int_cast(fid) == f as i64)
                        .unwrap_or(false)
                    || fid == "0";
                if in_scope {
                    entry_rules.push(pr);
                    break;
                }
            }
        }

        let feed_filter = crate::core::filters::EvalFilter {
            match_any_rule,
            inverse,
            rules: entry_rules.iter().map(|pr| pr.eval_rule.clone()).collect(),
            actions: vec![crate::core::filters::FilterAction::default()],
        };
        let outcome = crate::core::filters::eval_article_filters(
            std::slice::from_ref(&feed_filter),
            &title,
            &content,
            &link,
            &author,
            &tags,
        );
        if outcome.actions.is_empty() {
            continue;
        }

        // `strip_tags(preg_replace("/[\\r\\n\\t]/", "", $entry["content"]))` —
        // the tag-stripped text is what `mb_strpos` and the preview operate
        // on (mirrors the content/both preview path in
        // `RSSUtils::eval_article_filters`).
        let content_flat: String = util::strip_tags(
            &content
                .chars()
                .filter(|c| !matches!(c, '\r' | '\n' | '\t'))
                .collect::<String>(),
        );
        let mut content_preview = String::new();
        let mut matches: Vec<String> = Vec::new();
        let mut rules_names: Vec<String> = Vec::new();
        let mut entry_title = title.clone();

        for mr in &outcome.matched_rules {
            // `mb_substr(strip_tags($rule['regexp_matches'][0]), 0, 200)`
            let full_match = mr.regexp_matches.first().cloned().unwrap_or_default();
            let rule_regexp_match: String =
                util::strip_tags(&full_match).chars().take(200).collect();
            matches.push(rule_regexp_match.clone());

            let mut raw_map = mr
                .rule_index
                .and_then(|idx| entry_rules.get(idx))
                .and_then(|pr| pr.raw.as_object())
                .unwrap_or(&empty_map)
                .clone();
            // PHP: `$rule['inverse'] ??= false;` — the key is always present
            // here, so `_get_rule_name` always appends `(inverse)`.
            raw_map.entry("inverse").or_insert(Value::Bool(false));
            rules_names.push(get_rule_name(pool, uid, &raw_map, false).await);

            let rule_type = mr
                .rule_index
                .and_then(|idx| entry_rules.get(idx))
                .map(|pr| pr.eval_rule.rule_type)
                .unwrap_or(crate::core::filters::RuleType::Unknown);
            let preview = match rule_type {
                crate::core::filters::RuleType::Content | crate::core::filters::RuleType::Both => {
                    // `mb_strpos` (case-sensitive, char index) +
                    // `mb_substr(…, $match_index)`; a miss (e.g. case
                    // mismatch) previews from the start without `&hellip;`.
                    match content_flat.find(&rule_regexp_match) {
                        Some(byte_i) => {
                            let char_i = content_flat[..byte_i].chars().count();
                            let mut p =
                                util::truncate_string(&content_flat[byte_i..], 200, "&hellip;");
                            if char_i > 0 {
                                p = format!("&hellip;{p}");
                            }
                            p
                        }
                        None => util::truncate_string(&content_flat, 200, "&hellip;"),
                    }
                }
                crate::core::filters::RuleType::Link => link.clone(),
                crate::core::filters::RuleType::Author => author.clone(),
                crate::core::filters::RuleType::Tag => {
                    format!(
                        "<i class=\"material-icons\">label_outline</i> {}",
                        tags.join(", ")
                    )
                }
                _ => "&mdash;".to_string(),
            };
            content_preview = preview;

            let (mut hl_title, mut hl_content) = (false, false);
            match rule_type {
                crate::core::filters::RuleType::Both => {
                    hl_title = true;
                    hl_content = true;
                }
                crate::core::filters::RuleType::Title => hl_title = true,
                crate::core::filters::RuleType::Content
                | crate::core::filters::RuleType::Link
                | crate::core::filters::RuleType::Author
                | crate::core::filters::RuleType::Tag => hl_content = true,
                crate::core::filters::RuleType::Unknown => {}
            }
            // PHP: an empty highlight word (rule matched nothing textually —
            // e.g. via `inverse` — or the match stripped to nothing) makes
            // `Sanitizer::highlight_words` loop forever → the
            // max-execution-time fatal → 500 with an empty body.
            if rule_regexp_match.is_empty() && (hl_content || hl_title) {
                return Err(());
            }
            if hl_content {
                content_preview = crate::sanitizer::highlight_words_str(&content_preview, &matches);
            }
            if hl_title {
                entry_title = crate::sanitizer::highlight_words_str(&entry_title, &matches);
            }
        }

        // `mb_substr($entry['date_entered'], 0, 16)`
        let date = date_entered
            .map(|dt| crate::time::naive_to_pg_text(&dt))
            .unwrap_or_default();
        let date: String = date.chars().take(16).collect();

        items.push(json!({
            "title": entry_title,
            "feed_title": feed_title,
            "date": date,
            "content_preview": content_preview,
            "rules": rules_names,
        }));
    }

    Ok(json!({
        "pre_filtering_count": rows.len(),
        "items": items,
    }))
}

/// `Pref_Filters::index` — the filters tab page (the Dojo UI). The `search`
/// param (when present) is cleaned and stored in the session as
/// `prefs_filter_search`; otherwise the session value is used. Renders the page
/// (toolbar with the search form + the filter tree/store). The trailing
/// `PluginHost` hook is a no-op (no plugins) and `__()` is the identity, so the
/// button labels are their literal English text.
pub async fn index(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let filter_search = if let Some(v) = extra.get("search") {
        let s = util::clean_str(v.as_str().unwrap_or(""));
        sess.with_mut(|d| {
            d.set(
                "prefs_filter_search",
                crate::core::phpser::Value::Bytes(s.as_bytes().to_vec()),
            )
        });
        s
    } else {
        sess.data()
            .get_str("prefs_filter_search")
            .unwrap_or("")
            .to_string()
    };

    let mut ctx: Map<String, Value> = Map::new();
    ctx.insert(
        "filter_search".into(),
        Value::String(htmlspecialchars(&filter_search)),
    );
    // M7.22 item 7 — the `Pref_Filters::index` plugin chain (PHP site after
    // the tree panes, before the page's closing tag).
    ctx.insert(
        "pref_filters_tabs".into(),
        Value::String(
            crate::pluginhost::dispatch::prefs_chain(
                state,
                sess,
                crate::pluginhost::HOOK_PREFS_TAB,
                "prefFilters",
            )
            .await,
        ),
    );

    filters_index_env()
        .get_template("filters_index")
        .expect("filters_index template")
        .render(ctx)
        .expect("filters_index template render")
}

/// The minijinja environment holding `filters_index` (parsed once).
fn filters_index_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template(
            "filters_index",
            include_str!("../templates/filters_index.html.jinja"),
        )
        .expect("valid filters_index template");
        env
    })
}

// ---------------------------------------------------------------------------
// `Pref_Filters` write handlers (filter CRUD).
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// `Pref_Filters::filtersortreset` — reset every filter's order to 0.
pub async fn filtersortreset(state: &AppState, sess: &Sess) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    sqlx::query("UPDATE ttrss_filters2 SET order_id = 0 WHERE owner_uid = $1")
        .bind(uid)
        .execute(pool)
        .await?;
    Ok(())
}

/// `Pref_Filters::savefilterorder` — apply the reordered filter list (the
/// `payload` JSON: `{items: [{items: [{_reference: "FILTER:n"}, …]}, …]}`).
pub async fn savefilterorder(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let Some(payload) = extra.get("payload").and_then(Value::as_str) else {
        return Ok(());
    };
    let data: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let items = data.get("items").map(|items| {
        if items.is_array() {
            items.clone()
        } else {
            serde_json::from_str(items.as_str().unwrap_or("")).unwrap_or(Value::Null)
        }
    });
    let Some(items_arr) = items.and_then(|v| v.as_array().cloned()) else {
        return Ok(());
    };
    let Some(inner) = items_arr
        .first()
        .and_then(|f| f.get("items"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let mut index = 0;
    for item in inner {
        let ref_str = item.get("_reference").and_then(Value::as_str).unwrap_or("");
        let filter_id = util::php_int_cast(&ref_str.replace("FILTER", "").replace(':', "")) as i32;
        if filter_id > 0 {
            sqlx::query("UPDATE ttrss_filters2 SET order_id = $1 WHERE id = $2 AND owner_uid = $3")
                .bind(index)
                .bind(filter_id)
                .bind(uid)
                .execute(pool)
                .await?;
            index += 1;
        }
    }
    Ok(())
}

/// `Pref_Filters::remove` — delete the selected filters (rules/actions cascade).
pub async fn remove_filters(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = util::param_int_array(extra, "ids");
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM ttrss_filters2 WHERE owner_uid = $1 AND id = ANY($2)")
        .bind(uid)
        .bind(&ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// PHP truthiness for a decoded rule/action value (used for the `$rule` /
/// `$action` `if ($rule)` guards).
fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !(s.is_empty() || s == "0"),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Decode a `rule`/`action` request param: an array of JSON strings, each an
/// object. Returns the decoded objects (with `id` removed, deduped) — mirroring
/// `Pref_Filters::_save_rules_and_actions` parsing.
fn parse_json_objects(extra: &Map<String, Value>, name: &str) -> Vec<Value> {
    let Some(items) = extra.get(name).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    for it in items {
        let Some(s) = it.as_str() else { continue };
        let Ok(val) = serde_json::from_str::<Value>(s) else {
            continue;
        };
        if !matches!(val, Value::Object(_) | Value::Array(_)) {
            continue; // PHP: if (!is_array($rule)) continue;
        }
        let mut val = val;
        if let Value::Object(map) = &mut val {
            map.remove("id");
        }
        if !out.contains(&val) {
            out.push(val);
        }
    }
    out
}

/// `Pref_Filters::_save_rules_and_actions` — delete and re-insert a filter's
/// rules and actions from the `rule`/`action` request params.
async fn save_rules_and_actions(
    tx: &mut sqlx::PgConnection,
    filter_id: i32,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM ttrss_filters2_rules WHERE filter_id = $1")
        .bind(filter_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM ttrss_filters2_actions WHERE filter_id = $1")
        .bind(filter_id)
        .execute(&mut *tx)
        .await?;
    if filter_id == 0 {
        return Ok(());
    }
    let rules = parse_json_objects(extra, "rule");
    let actions = parse_json_objects(extra, "action");

    for rule in &rules {
        if !json_truthy(rule) {
            continue;
        }
        let reg_exp = rule
            .get("reg_exp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let inverse = rule.get("inverse").is_some_and(|v| !v.is_null()); // isset($rule["inverse"])
        let filter_type = util::clean_int(rule.get("filter_type")) as i32;
        let match_on = rule
            .get("feed_id")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, filter_type, feed_id, cat_id, match_on, inverse) VALUES ($1, $2, $3, NULL, NULL, $4, $5)")
            .bind(filter_id)
            .bind(&reg_exp)
            .bind(filter_type)
            .bind(&match_on)
            .bind(inverse)
            .execute(&mut *tx)
            .await?;
    }
    for action in &actions {
        if !json_truthy(action) {
            continue;
        }
        let action_id = util::clean_int(action.get("action_id")) as i32;
        let action_param = action
            .get("action_param")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let action_param_label = action
            .get("action_param_label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let action_param = match action_id as i64 {
            ACTION_LABEL => action_param_label,
            ACTION_SCORE => util::php_int_cast(&action_param.replace('+', "")).to_string(),
            ACTION_TAG | ACTION_REMOVE_TAG => {
                let tags: Vec<String> = action_param.split(',').map(|s| s.to_string()).collect();
                util::normalize_categories(&tags).join(", ")
            }
            _ => action_param,
        };
        sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) VALUES ($1, $2, $3)")
            .bind(filter_id)
            .bind(action_id)
            .bind(&action_param)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

/// `Pref_Filters::editSave` — update the filter row and re-save its rules and
/// actions. (Matches PHP: no owner check on the `id`.)
pub async fn edit_save(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let mut tx = pool.begin().await?;
    let filter_id = util::clean_int(extra.get("id")) as i32;
    if filter_id > 0 {
        let title = extra
            .get("title")
            .and_then(util::scalar_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let enabled = util::checkbox(extra, "enabled");
        let match_any = util::checkbox(extra, "match_any_rule");
        let inverse = util::checkbox(extra, "inverse");
        sqlx::query("UPDATE ttrss_filters2 SET enabled = $1, match_any_rule = $2, inverse = $3, title = $4 WHERE id = $5 AND owner_uid = $6")
            .bind(enabled)
            .bind(match_any)
            .bind(inverse)
            .bind(&title)
            .bind(filter_id)
            .bind(uid)
            .execute(&mut *tx)
            .await?;
        save_rules_and_actions(&mut tx, filter_id, extra).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `Pref_Filters::_clone_rules_and_actions` — copy the rules and actions of
/// `src_filter_id` into `filter_id`.
async fn clone_rules_and_actions(
    tx: &mut sqlx::PgConnection,
    filter_id: i32,
    src_filter_id: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO ttrss_filters2_rules (filter_id, reg_exp, inverse, filter_type, feed_id, cat_id, match_on, cat_filter) SELECT $1, reg_exp, inverse, filter_type, feed_id, cat_id, match_on, cat_filter FROM ttrss_filters2_rules WHERE filter_id = $2")
        .bind(filter_id)
        .bind(src_filter_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO ttrss_filters2_actions (filter_id, action_id, action_param) SELECT $1, action_id, action_param FROM ttrss_filters2_actions WHERE filter_id = $2")
        .bind(filter_id)
        .bind(src_filter_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The shared base-filter insert + rules/actions (the body of
/// `Pref_Filters::add`). `src_filter_id` `Some(..)` clones; `None` saves the
/// posted rules/actions.
#[allow(clippy::too_many_arguments)]
async fn do_add(
    tx: &mut sqlx::PgConnection,
    uid: i64,
    src_filter_id: Option<i32>,
    title: &str,
    enabled: bool,
    match_any: bool,
    inverse: bool,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let new_id: Option<i32> = sqlx::query_scalar(
        "INSERT INTO ttrss_filters2 (owner_uid, match_any_rule, enabled, title, inverse) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(uid)
    .bind(match_any)
    .bind(enabled)
    .bind(title)
    .bind(inverse)
    .fetch_one(&mut *tx)
    .await?;
    let Some(new_id) = new_id else {
        return Ok(());
    };
    match src_filter_id {
        Some(src) => clone_rules_and_actions(&mut *tx, new_id, src).await?,
        None => save_rules_and_actions(&mut *tx, new_id, extra).await?,
    }
    Ok(())
}

/// `Pref_Filters::add` — create a new filter from the posted params.
pub async fn add_filter(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let title = extra
        .get("title")
        .and_then(util::scalar_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let enabled = util::checkbox(extra, "enabled");
    let match_any = util::checkbox(extra, "match_any_rule");
    let inverse = util::checkbox(extra, "inverse");
    let mut tx = pool.begin().await?;
    do_add(
        &mut tx, uid, None, &title, enabled, match_any, inverse, extra,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `Pref_Filters::clone` — clone the selected filters (one transaction each).
pub async fn clone_filters(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = util::param_int_array(extra, "ids");
    if ids.is_empty() {
        return Ok(());
    }
    let new_title = if ids.len() == 1 {
        extra
            .get("new_filter_title")
            .and_then(util::scalar_str)
            .map(|s| s.trim().to_string())
    } else {
        None
    };
    let srcs = sqlx::query("SELECT id, title, match_any_rule, inverse FROM ttrss_filters2 WHERE owner_uid = $1 AND id = ANY($2) ORDER BY id")
        .bind(uid)
        .bind(&ids)
        .fetch_all(pool)
        .await?;
    for s in &srcs {
        let src_id: i32 = s.get(0);
        let src_title: String = s.get(1);
        let src_match_any: bool = s.get(2);
        let src_inverse: bool = s.get(3);
        let title = new_title
            .clone()
            .unwrap_or_else(|| format!("Clone of {src_title}"));
        let mut tx = pool.begin().await?;
        do_add(
            &mut tx,
            uid,
            Some(src_id),
            &title,
            false,
            src_match_any,
            src_inverse,
            &Map::new(),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// `Pref_Filters::join` — merge the selected filters into the first one (the
/// base), then dedupe its rules/actions.
pub async fn join_filters(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = util::param_int_array(extra, "ids");
    if ids.is_empty() {
        return Ok(());
    }
    // fail early if any provided filter id isn't owned by the user
    let unowned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_filters2 WHERE id = ANY($1) AND owner_uid <> $2",
    )
    .bind(&ids)
    .bind(uid)
    .fetch_one(pool)
    .await?;
    if unowned > 0 {
        return Ok(());
    }
    if ids.len() > 1 {
        let base_id = ids[0];
        let remaining = &ids[1..];
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE ttrss_filters2_rules SET filter_id = $1 WHERE filter_id = ANY($2)")
            .bind(base_id)
            .bind(remaining)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE ttrss_filters2_actions SET filter_id = $1 WHERE filter_id = ANY($2)")
            .bind(base_id)
            .bind(remaining)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM ttrss_filters2 WHERE id = ANY($1)")
            .bind(remaining)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE ttrss_filters2 SET match_any_rule = true WHERE id = $1")
            .bind(base_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        optimize(pool, base_id as i32).await?;
    }
    Ok(())
}

/// `Pref_Filters::_optimize` — dedupe a filter's rules and actions: keep the
/// first occurrence (by id) of each unique row, delete the rest.
async fn optimize(pool: &sqlx::PgPool, id: i32) -> Result<(), sqlx::Error> {
    use std::collections::HashSet;
    /// The rule dedupe signature (all non-id columns).
    type RuleSig = (
        i32,
        String,
        bool,
        i32,
        Option<i32>,
        Option<i32>,
        Option<String>,
        bool,
    );
    let mut tx = pool.begin().await?;

    // actions: unique on (filter_id, action_id, action_param).
    let actions = sqlx::query(
        "SELECT id, filter_id, action_id, action_param FROM ttrss_filters2_actions WHERE filter_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut seen: HashSet<(i32, i32, String)> = HashSet::new();
    let mut dupe_ids: Vec<i32> = Vec::new();
    for a in &actions {
        let aid: i32 = a.get(0);
        let filter_id: i32 = a.get(1);
        let action_id: i32 = a.get(2);
        let action_param: String = a.get(3);
        if !seen.insert((filter_id, action_id, action_param)) {
            dupe_ids.push(aid);
        }
    }
    if !dupe_ids.is_empty() {
        sqlx::query("DELETE FROM ttrss_filters2_actions WHERE id = ANY($1)")
            .bind(&dupe_ids)
            .execute(&mut *tx)
            .await?;
    }

    // rules: unique on all non-id columns.
    let rules = sqlx::query(
        "SELECT id, filter_id, reg_exp, inverse, filter_type, feed_id, cat_id, match_on, cat_filter \
         FROM ttrss_filters2_rules WHERE filter_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut seen: HashSet<RuleSig> = HashSet::new();
    let mut dupe_ids: Vec<i32> = Vec::new();
    for r in &rules {
        let rid: i32 = r.get(0);
        let filter_id: i32 = r.get(1);
        let reg_exp: String = r.get(2);
        let inverse: bool = r.get(3);
        let filter_type: i32 = r.get(4);
        let feed_id: Option<i32> = r.get(5);
        let cat_id: Option<i32> = r.get(6);
        let match_on: Option<String> = r.get(7);
        let cat_filter: bool = r.get(8);
        if !seen.insert((
            filter_id,
            reg_exp,
            inverse,
            filter_type,
            feed_id,
            cat_id,
            match_on,
            cat_filter,
        )) {
            dupe_ids.push(rid);
        }
    }
    if !dupe_ids.is_empty() {
        sqlx::query("DELETE FROM ttrss_filters2_rules WHERE id = ANY($1)")
            .bind(&dupe_ids)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}
