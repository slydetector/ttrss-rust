//! Port of `Digest::prepare_headlines_digest` (classes/Digest.php) — the
//! preview of a headline digest, rendered from the two MiniTemplator files
//! `digest_template_html.txt` and `digest_template.txt`.
//!
//! Returns `[html, count, affected_ids, text]`, matching the PHP return.
//! The block structure is reproduced exactly: an un-added feed/article block
//! emits nothing, so a digest with no headlines is just the header + footer.

use std::collections::HashMap;

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::Value;
use sqlx::AssertSqlSafe;
use sqlx::Row as _;

use crate::prefs;
use crate::time::TzCtx;

/// One digest article row.
#[allow(dead_code)]
struct Row {
    title: String,
    feed_title: String,
    link: String,
    updated: String,
    /// `&hellip;` ellipsis (the default) — rendered by the HTML template. The
    /// plain-text template only emits title/updated/link, so no `...` variant
    /// is needed (PHP sets one on the text Templator, but
    /// `digest_template.txt` never references `ARTICLE_EXCERPT`).
    excerpt: String,
    labels: String,
    ref_id: i64,
    score: i64,
    date_updated: String,
}

/// `Digest::prepare_headlines_digest(user_id, days, limit)`.
pub async fn prepare_headlines_digest(
    state: &crate::AppState,
    uid: i32,
    days: i64,
    limit: i64,
    self_url: &str,
) -> Value {
    let db = state.db.pool();
    let rows = prefs::load_rows(&state.db, uid as i64, None).await;

    let tz_string = prefs::resolve(prefs::USER_TIMEZONE, &rows, None)
        .as_str()
        .to_string();
    let tz_string = if tz_string == "Automatic" {
        "GMT".to_string()
    } else {
        tz_string
    };
    let min_score = prefs::resolve(prefs::DIGEST_MIN_SCORE, &rows, None).as_int();
    let enable_cats = prefs::resolve(prefs::ENABLE_FEED_CATS, &rows, None).as_bool();

    let (cur_date, cur_time) = cur_date_time(&tz_string);

    // The headline query (mirrors the PHP SQL; `NOW() - INTERVAL '.. day'`).
    let lines = sqlx::query(AssertSqlSafe(format!(
        "SELECT ttrss_entries.title,
                ttrss_feeds.title AS feed_title,
                COALESCE(ttrss_feed_categories.title, 'Uncategorized') AS cat_title,
                ttrss_entries.date_updated,
                ttrss_user_entries.ref_id,
                ttrss_entries.link,
                ttrss_user_entries.score,
                ttrss_entries.content,
                SUBSTRING_FOR_DATE(ttrss_feeds.last_updated, 1, 19) AS last_updated
         FROM ttrss_user_entries, ttrss_entries, ttrss_feeds
         LEFT JOIN ttrss_feed_categories ON (cat_id = ttrss_feed_categories.id)
         WHERE ref_id = ttrss_entries.id
           AND feed_id = ttrss_feeds.id
           AND include_in_digest = true
           AND ttrss_entries.date_updated > NOW() - INTERVAL '{days} day'
           AND ttrss_user_entries.owner_uid = $1
           AND unread = true
           AND score >= $2
         ORDER BY ttrss_feed_categories.title, ttrss_feeds.title, score DESC, ttrss_entries.date_updated DESC
         LIMIT $3"),
    ))
    .bind(uid)
    .bind(min_score)
    .bind(limit)
    .fetch_all(db)
    .await
    .expect("digest headline query");

    let mut articles: Vec<Row> = Vec::new();
    let tz = TzCtx {
        user_tz: tz_string.clone(),
        client_tz_offset: 0,
        short_date_format: prefs::resolve(prefs::SHORT_DATE_FORMAT, &rows, None)
            .as_str()
            .to_string(),
        long_date_format: prefs::resolve(prefs::LONG_DATE_FORMAT, &rows, None)
            .as_str()
            .to_string(),
    };
    for l in lines {
        let title = l.get::<String, _>(0);
        let feed_title = l.get::<String, _>(1);
        let cat_title = l.get::<String, _>(2);
        let date_updated = l.get::<chrono::NaiveDateTime, _>(3);
        let date_updated = date_updated.format("%Y-%m-%d %H:%M:%S").to_string();
        // ref_id/score are INT4 — decode as i32 (strict decoding), widen at
        // the boundary. (The old i64 decode errored on every row, and the
        // query's `unwrap_or_default()` turned that into an always-empty
        // digest — now the query `.expect()`s.)
        let ref_id = l.get::<i32, _>(4) as i64;
        let link = l.get::<String, _>(5);
        let score = l.get::<i32, _>(6) as i64;
        let content = l.get::<String, _>(7);
        let last_updated = l.get::<String, _>(8);

        let mut feed_title = feed_title;
        if enable_cats {
            feed_title = format!("{cat_title} / {feed_title}");
        }
        let updated = crate::time::make_local_datetime(Some(&last_updated), false, &tz);
        let excerpt =
            crate::util::truncate_string(&crate::util::strip_tags(&content), 300, "&hellip;");
        // `Article::_get_labels(ref_id, user_id)` — the digest shows the
        // comma-joined label captions in both templates.
        let labels = crate::article::get_labels(db, uid as i64, ref_id, Some(uid as i64))
            .await
            .iter()
            .filter_map(|row| row.get(1).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        articles.push(Row {
            title,
            feed_title,
            link,
            updated,
            excerpt,
            labels,
            ref_id,
            score,
            date_updated,
        });
    }

    // Group by feed, preserving the (category, feed, score DESC, date DESC) order.
    let mut feeds: Vec<(String, Vec<Row>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in articles {
        let key = r.feed_title.clone();
        let pos = match index.get(&key) {
            Some(&p) => p,
            None => {
                feeds.push((key.clone(), Vec::new()));
                let p = feeds.len() - 1;
                index.insert(key, p);
                p
            }
        };
        feeds[pos].1.push(r);
    }

    let count = feeds.iter().map(|(_, a)| a.len()).sum::<usize>();
    let affected_ids: Vec<Value> = feeds
        .iter()
        .flat_map(|(_, a)| a.iter().map(|r| Value::from(r.ref_id)))
        .collect();

    let html = render_html(&cur_date, &cur_time, self_url, &feeds);
    let text = render_text(&cur_date, &cur_time, self_url, &feeds);

    Value::Array(vec![
        Value::String(html),
        Value::from(count),
        Value::Array(affected_ids),
        Value::String(text),
    ])
}

/// `CUR_DATE` (`Y/m/d`) and `CUR_TIME` (`G:i`, 24-hour, no leading-zero hour)
/// in the user's timezone, from `TimeHelper::convert_timestamp` + `date()`.
fn cur_date_time(tz_string: &str) -> (String, String) {
    let now: DateTime<Tz> = Utc::now().with_timezone(&tz_string.parse().unwrap_or(Tz::GMT));
    (
        now.format("%Y/%m/%d").to_string(),
        format!("{}:{}", now.hour(), now.format("%M")),
    )
}

/// `digest_template_html.txt` with the feed/article blocks emitted only when
/// they were added (un-added blocks emit nothing).
fn render_html(
    cur_date: &str,
    cur_time: &str,
    self_url: &str,
    feeds: &[(String, Vec<Row>)],
) -> String {
    let mut out = String::new();
    out.push_str("\n<h1>New headlines for last 24 hours, as of ");
    out.push_str(cur_date);
    out.push(' ');
    out.push_str(cur_time);
    out.push_str("</h1>\n<hr/>\n");
    for (feed_title, articles) in feeds {
        out.push_str("<h2>");
        out.push_str(feed_title);
        out.push_str("</h2>\n\n<ul>\n");
        for a in articles {
            out.push_str("<li><a href=\"");
            out.push_str(&a.link);
            out.push_str("\">");
            out.push_str(&a.title);
            out.push_str("</a> &mdash; <strong>");
            out.push_str(&a.updated);
            out.push_str("</strong>\n<div><em>");
            out.push_str(&a.labels);
            out.push_str("</em></div>\n<div>");
            out.push_str(&a.excerpt);
            out.push_str("</div>\n");
        }
        out.push_str("</ul>\n");
    }
    out.push_str(
        "\n\n<hr>\n\n<em style='color : gray'>To unsubscribe, visit your configuration options or \
         contact instance owner.<br/>\nSent by tt-rss mailer daemon at ",
    );
    out.push_str(self_url);
    out.push('.');
    out.push_str("</em>\n");
    // The main (implicit) block wraps the digest block, so the trailing
    // newline after `<!-- $EndBlock digest -->` is emitted too.
    out.push('\n');
    out
}

/// `digest_template.txt` (the plain-text digest), same block semantics.
fn render_text(
    cur_date: &str,
    cur_time: &str,
    self_url: &str,
    feeds: &[(String, Vec<Row>)],
) -> String {
    let mut out = String::new();
    out.push_str("\nNew headlines for last 24 hours, as of ");
    out.push_str(cur_date);
    out.push(' ');
    out.push_str(cur_time);
    out.push_str("\n==============================================================\n");
    for (feed_title, articles) in feeds {
        out.push_str(feed_title);
        out.push_str("\n\n");
        for a in articles {
            out.push_str("* ");
            out.push_str(&a.title);
            out.push_str(" - ");
            out.push_str(&a.updated);
            out.push_str("\n  ");
            out.push_str(&a.link);
            out.push('\n');
        }
    }
    out.push_str(
        "\n-- \nTo unsubscribe, visit your configuration options or contact instance owner.\n",
    );
    out.push_str("Sent by tt-rss mailer daemon at ");
    out.push_str(self_url);
    out.push_str(".\n");
    out.push('\n');
    out
}
