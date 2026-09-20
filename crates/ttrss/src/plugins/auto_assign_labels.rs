//! `auto_assign_labels` (M7.10) — "Assign labels automatically based on article
//! title, content, and tags" (fox).
//!
//! At [`HOOK_ARTICLE_FILTER`], for each of the owner's labels, if the label's
//! caption appears (whole-word, case-insensitive) in the article's tags +
//! content + title, the label is added to the article's label list (dedup'd
//! against what's already there — the PHP's `labels_contains_caption`). The
//! pipeline then stores the article's label list (existing + plugin-added), so
//! matching labels stick to the article.
//!
//! The reference's `get_all_labels_filter_format` queries `ttrss_labels2`
//! directly (the owner's label *definitions*); `Labels::label_to_feed_id` is the
//! port's [`crate::labels::label_to_feed_id`].

use regex::Regex;
use serde_json::{Value, json};
use sqlx::Row;

use crate::labels;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::HOOK_ARTICLE_FILTER;
use crate::util::strip_tags;

/// The `auto_assign_labels` plugin (the PHP's `Auto_Assign_Labels`).
#[derive(Clone, Copy)]
pub struct AutoAssignLabels;

impl Plugin for AutoAssignLabels {
    fn name(&self) -> &'static str {
        "auto_assign_labels"
    }
    fn class_name(&self) -> &'static str {
        "Auto_Assign_Labels"
    }
    fn about(&self) -> About {
        About::new(
            "Assign labels automatically based on article title, content, and tags",
            "fox",
        )
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_ARTICLE_FILTER);
    }

    /// `hook_article_filter` — add each owner label whose caption matches the
    /// article (whole-word, case-insensitive) to the article's label list.
    fn hook_article_filter<'a, 'h: 'a>(
        &'a self,
        host: &'a mut HostCtx<'h>,
        article: &'a mut Value,
    ) -> BoxFuture<'a, Value> {
        let pool = host.pool;
        Box::pin(async move {
            let owner: i32 = article
                .get("owner_uid")
                .and_then(Value::as_i64)
                .unwrap_or(0) as i32;
            let all_labels = get_all_labels(pool, owner).await;
            if all_labels.is_empty() {
                return article.clone();
            }
            let tags_str = article
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let content = article.get("content").and_then(Value::as_str).unwrap_or("");
            let title = article.get("title").and_then(Value::as_str).unwrap_or("");
            // The PHP's `"$tags_str " . strip_tags($article["content"]) . " " . $article["title"]`.
            let haystack = format!("{tags_str} {} {}", strip_tags(content), title);

            let mut cur = article
                .get("labels")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            for label in &all_labels {
                let caption = label.get(1).and_then(Value::as_str).unwrap_or("");
                if caption.is_empty() || labels_contains_caption(&cur, caption) {
                    continue;
                }
                if caption_word_match(caption, &haystack) {
                    if let Some(arr) = cur.as_array_mut() {
                        arr.push(label.clone());
                    }
                }
            }
            article["labels"] = cur;
            article.clone()
        })
    }
}

/// `get_all_labels_filter_format` — the owner's label definitions as
/// `[feed_id, caption, fg_color, bg_color]` (the PHP's `_get_labels` shape).
async fn get_all_labels(pool: &sqlx::PgPool, owner_uid: i32) -> Vec<Value> {
    let rows = sqlx::query(
        "SELECT id, caption, fg_color, bg_color FROM ttrss_labels2 WHERE owner_uid = $1",
    )
    .bind(owner_uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|row| {
            let id: i32 = row.get(0);
            json!([
                labels::label_to_feed_id(id as i64),
                row.get::<String, _>(1),
                row.get::<String, _>(2),
                row.get::<String, _>(3),
            ])
        })
        .collect()
}

/// `RSSUtils::labels_contains_caption` — true when a label's caption matches.
fn labels_contains_caption(labels: &Value, caption: &str) -> bool {
    labels
        .as_array()
        .is_some_and(|arr| arr.iter().any(|l| l.get(1).and_then(Value::as_str) == Some(caption)))
}

/// The PHP's `preg_match("/\b$caption\b/i", …)` — a case-insensitive whole-word
/// match of `$caption` in `$haystack`.
fn caption_word_match(caption: &str, haystack: &str) -> bool {
    let pattern = format!(r"(?i)\b{}\b", regex::escape(caption));
    Regex::new(&pattern).map(|re| re.is_match(haystack)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `labels_contains_caption` — matches on the caption (index 1).
    #[test]
    fn contains_caption_matches_on_index_1() {
        let labels = json!([[1, "Work", "#000", "#fff"], [2, "News", "#000", "#fff"]]);
        assert!(labels_contains_caption(&labels, "Work"));
        assert!(labels_contains_caption(&labels, "News"));
        assert!(!labels_contains_caption(&labels, "Home"));
        assert!(!labels_contains_caption(&json!([]), "Work"));
    }

    /// `caption_word_match` — whole-word, case-insensitive (the PHP's `\b…\b/i`).
    #[test]
    fn word_match_is_whole_word_and_case_insensitive() {
        assert!(caption_word_match("Work", "I did work on the project"));
        assert!(caption_word_match("Work", "WORK in progress"));
        // Not a whole word: "Work" inside "Workshop" / "Network" doesn't match.
        assert!(!caption_word_match("Work", "Workshop hours"));
        assert!(!caption_word_match("Work", "network outages"));
        // Special characters are escaped (not treated as regex): a literal '.'
        // matches '.', not any char.
        assert!(caption_word_match("a.b", "has a.b here"));
        assert!(!caption_word_match("a.b", "has axb here"));
    }

    fn method_surface() {
        let p = AutoAssignLabels;
        assert_eq!(p.name(), "auto_assign_labels");
        assert_eq!(p.class_name(), "Auto_Assign_Labels");
        assert_eq!(p.api_version(), 2);
        assert_eq!(p.about().description, "Assign labels automatically based on article title, content, and tags");
        assert_eq!(p.about().author, "fox");
    }
    #[test]
    fn metadata() {
        method_surface();
    }
}
