//! `Pref_Filters` backend read handlers (the filters UI in
//! `backend.php?op=Pref_Filters`).

use serde_json::{json, Value};
use sqlx::Row;

use crate::prefs;
use crate::time::{make_local_datetime, TzCtx};
use crate::session::Sess;
use crate::util::{htmlspecialchars, t_sprintf};
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
                    user_tz: prefs::resolve("USER_TIMEZONE", &rows, profile).as_str().to_string(),
                    client_tz_offset: data.get_int("clientTzOffset").unwrap_or(0),
                    short_date_format: prefs::resolve("SHORT_DATE_FORMAT", &rows, profile).as_str().to_string(),
                    long_date_format: prefs::resolve("LONG_DATE_FORMAT", &rows, profile).as_str().to_string(),
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

