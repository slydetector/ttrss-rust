//! `Pref_Filters` backend read handlers (the filters UI in
//! `backend.php?op=Pref_Filters`).

use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::prefs;
use crate::time::{make_local_datetime, TzCtx};
use crate::session::Sess;
use crate::util::{htmlspecialchars, t_sprintf};
use crate::util;
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
fn action_description(action_id: i64) -> Option<&'static str> {
    Some(match action_id {
        1 => "Delete article",
        2 => "Mark as read",
        3 => "Set starred",
        4 => "Assign tags",
        5 => "Publish article",
        6 => "Modify score",
        7 => "Assign label",
        8 => "Stop / Do nothing",
        9 => "Invoke plugin",
        10 => "Ignore tags",
        _ => return None,
    })
}

/// `Pref_Filters::$filter_types` (filter_type → description).
fn filter_type_description(t: i64) -> Option<&'static str> {
    Some(match t {
        1 => "Title",
        2 => "Content",
        3 => "Title or Content",
        4 => "Link",
        6 => "Author",
        7 => "Article Tags",
        _ => return None,
    })
}

/// `Pref_Filters::_get_action_name`.
fn action_name(action_id: i64, action_param: &str) -> String {
    let base = match action_description(action_id) {
        Some(d) => d.to_string(),
        None => t_sprintf("Unknown action: %d", &[&action_id.to_string()]),
    };
    // (ACTION_PLUGIN is skipped — no plugin filter actions in the port.)
    if PARAM_ACTIONS.contains(&action_id) {
        format!("{base}: {action_param}")
    } else {
        base
    }
}

/// `Pref_Filters::_get_details` — `{title_summary, actions_summary}` for a filter.
async fn filter_details(pool: &sqlx::PgPool, id: i32) -> (String, String) {
    let (title, match_any, inverse, num_rules): (String, bool, bool, i64) =
        sqlx::query_as(
            "SELECT f.title, f.match_any_rule, f.inverse, COUNT(DISTINCT r.id) \
             FROM ttrss_filters2 AS f \
             LEFT JOIN ttrss_filters2_rules AS r ON r.filter_id = f.id \
             WHERE f.id = $1 GROUP BY f.title, f.match_any_rule, f.inverse",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| ("[No caption]".to_string(), false, false, 0));

    let title = if title.is_empty() {
        "[No caption]".to_string()
    } else {
        title
    };

    // title_summary: "%s (N rule[rules])" + optional "matches any rule" / "inverse".
    let mut parts: Vec<String> = Vec::new();
    parts.push(if num_rules == 1 {
        format!("{title} ({num_rules} rule)")
    } else {
        format!("{title} ({num_rules} rules)")
    });
    if match_any {
        parts.push("matches any rule".to_string());
    }
    if inverse {
        parts.push("inverse".to_string());
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
        summary.push(format!("<li>{}</li>", htmlspecialchars(&action_name(action_id, &action_param))));
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
        let noun = if not_shown == 1 { "(+%d action)" } else { "(+%d actions)" };
        summary.push(format!(
            "<li class=\"text-muted\"><em>{}</em></li>",
            t_sprintf(noun, &[&not_shown.to_string()])
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
        let desc = filter_type_description(filter_type).unwrap_or("");
        rv.push_str(&format!(
            "<li class='{inverse_class}'>{}</li>",
            t_sprintf(
                "%s on %s in %s %s",
                &[
                    &htmlspecialchars(&reg_exp),
                    desc,
                    &htmlspecialchars(&where_str),
                    &if inverse { "(inverse)".to_string() } else { String::new() },
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
        if !mo.is_empty() {
            if let Ok(referenced) = serde_json::from_str::<Vec<Value>>(mo) {
                let mut parts: Vec<String> = Vec::new();
                for f in &referenced {
                    if let Some(fid) = f.as_str() {
                        if let Some(rest) = fid.strip_prefix("CAT:") {
                            parts.push(
                                crate::feeds::category_title(pool, rest.parse().unwrap_or(0), uid).await,
                            );
                        } else if fid.is_empty() {
                            parts.push("All feeds".to_string());
                        } else {
                            parts.push(
                                crate::feeds::feed_title(pool, fid.parse().unwrap_or(0), uid)
                                    .await,
                            );
                        }
                    }
                }
                return parts.join(", ");
            }
        }
    }
    if cat_filter {
        crate::feeds::category_title(pool, cat_id.unwrap_or(0), uid).await
    } else if let Some(fid) = feed_id {
        crate::feeds::feed_title(pool, fid, uid).await
    } else {
        "All feeds".to_string()
    }
}

/// `Pref_Filters::getfiltertree`.
pub async fn get_filter_tree(state: &AppState, sess: &Sess) -> Value {
    let pool = state.db.pool();
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let filter_search = data.get_str("prefs_filter_search").unwrap_or("").to_string();

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

        let bare_name = if title.is_empty() { "[No caption]".to_string() } else { title };
        let (title_summary, actions_summary) = filter_details(pool, id).await;
        let rules = rules_list(pool, id, uid).await;

        let last_triggered_json = match last_triggered.as_deref().filter(|t| !t.is_empty()) {
            Some(ts) => {
                let rows = prefs::load_rows(&state.db, uid, profile).await;
                let tz = TzCtx {
                    user_tz: prefs::resolve(prefs::USER_TIMEZONE, &rows, profile).as_str().to_string(),
                    client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
                    short_date_format: prefs::resolve(prefs::SHORT_DATE_FORMAT, &rows, profile).as_str().to_string(),
                    long_date_format: prefs::resolve(prefs::LONG_DATE_FORMAT, &rows, profile).as_str().to_string(),
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
            "name": "Filters",
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

// ---------------------------------------------------------------------------
// `Pref_Filters` write handlers (filter CRUD).
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
pub async fn savefilterorder(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
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
    let Some(inner) = items_arr.first().and_then(|f| f.get("items")).and_then(Value::as_array) else {
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
pub async fn remove_filters(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
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
        let Ok(val) = serde_json::from_str::<Value>(s) else { continue };
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
        let reg_exp = rule.get("reg_exp").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let inverse = rule.get("inverse").is_some_and(|v| !v.is_null()); // isset($rule["inverse"])
        let filter_type = util::clean_int(rule.get("filter_type")) as i32;
        let match_on = rule.get("feed_id").map(|v| v.to_string()).unwrap_or_else(|| "null".to_string());
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
        let action_param = action.get("action_param").and_then(Value::as_str).unwrap_or("").to_string();
        let action_param_label = action.get("action_param_label").and_then(Value::as_str).unwrap_or("").to_string();
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
pub async fn edit_save(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let mut tx = pool.begin().await?;
    let filter_id = util::clean_int(extra.get("id")) as i32;
    if filter_id > 0 {
        let title = extra.get("title").and_then(util::scalar_str).unwrap_or_default().trim().to_string();
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
pub async fn add_filter(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let title = extra.get("title").and_then(util::scalar_str).unwrap_or_default().trim().to_string();
    let enabled = util::checkbox(extra, "enabled");
    let match_any = util::checkbox(extra, "match_any_rule");
    let inverse = util::checkbox(extra, "inverse");
    let mut tx = pool.begin().await?;
    do_add(&mut tx, uid, None, &title, enabled, match_any, inverse, extra).await?;
    tx.commit().await?;
    Ok(())
}

/// `Pref_Filters::clone` — clone the selected filters (one transaction each).
pub async fn clone_filters(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
    let uid = sess.data().uid().unwrap_or(0);
    let pool = state.db.pool();
    let ids = util::param_int_array(extra, "ids");
    if ids.is_empty() {
        return Ok(());
    }
    let new_title = if ids.len() == 1 {
        extra.get("new_filter_title").and_then(util::scalar_str).map(|s| s.trim().to_string())
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
        let title = new_title.clone().unwrap_or_else(|| format!("Clone of {src_title}"));
        let mut tx = pool.begin().await?;
        do_add(&mut tx, uid, Some(src_id), &title, false, src_match_any, src_inverse, &Map::new()).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// `Pref_Filters::join` — merge the selected filters into the first one (the
/// base), then dedupe its rules/actions.
pub async fn join_filters(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Result<(), sqlx::Error> {
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
        optimize(pool, base_id).await?;
    }
    Ok(())
}

/// `Pref_Filters::_optimize` — dedupe a filter's rules and actions: keep the
/// first occurrence (by id) of each unique row, delete the rest.
async fn optimize(pool: &sqlx::PgPool, id: i32) -> Result<(), sqlx::Error> {
    use std::collections::HashSet;
    /// The rule dedupe signature (all non-id columns).
    type RuleSig = (i32, String, bool, i32, Option<i32>, Option<i32>, Option<String>, bool);
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
        if !seen.insert((filter_id, reg_exp, inverse, filter_type, feed_id, cat_id, match_on, cat_filter)) {
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

