//! Oracle-verified SQL fragments for `search_to_sql` (the port of
//! `Feeds::_search_to_sql`).
//!
//! The expected fragments below are the exact outputs of the real PHP 8.5
//! `_search_to_sql` (invoked via reflection in the oracle app container,
//! uid 1, no profile, empty search language). The fixture labels 3-8 are
//! seeded to match the oracle DB so `label:Inbox` resolves to id 4.
//!
//! Date-relative `@date` cases are checked separately against dates
//! computed from the current UTC day (the oracle table recorded on
//! 2026-09-08: yesterday=09-07, last Monday=09-07).

mod common;

use chrono::Datelike;
use test_db::template::with_test_db;
use ttrss::search;

/// (input, expected query_part, expected words)
const CASES: &[(&str, &str, &[&str])] = &[
    ("", "false", &[]),
    ("   ", "false", &[]),
    ("\"\"", "false", &[]),
    (
        "hello",
        "(tsvector_combined @@ to_tsquery('', 'hello'))",
        &["hello"],
    ),
    (
        "hello world",
        "(tsvector_combined @@ to_tsquery('', 'hello & world'))",
        &["hello", "world"],
    ),
    (
        "hello  world",
        "(tsvector_combined @@ to_tsquery('', 'hello & world'))",
        &["hello", "world"],
    ),
    (
        "-hello",
        "(tsvector_combined @@ to_tsquery('', '!hello'))",
        &[],
    ),
    (
        "hello | world",
        "(tsvector_combined @@ to_tsquery('', 'hello | world'))",
        &["hello", "world"],
    ),
    (
        "hello & world",
        "(tsvector_combined @@ to_tsquery('', 'hello & world'))",
        &["hello", "world"],
    ),
    (
        "!(hello | world)",
        "(tsvector_combined @@ to_tsquery('', '!(hello | world)'))",
        &["!(hello", "world)"],
    ),
    (
        "\"one two three\"",
        "(tsvector_combined @@ to_tsquery('', '(one <-> two <-> three)'))",
        &["one two three"],
    ),
    (
        "-\"one two\"",
        "(tsvector_combined @@ to_tsquery('', '!(one <-> two)'))",
        &[],
    ),
    (
        "title:foo",
        "( (LOWER(ttrss_entries.title) LIKE '%foo%'))",
        &[],
    ),
    (
        "-title:foo",
        "(NOT (LOWER(ttrss_entries.title) LIKE '%foo%'))",
        &[],
    ),
    (
        "title:\"foo bar\"",
        "( (LOWER(ttrss_entries.title) LIKE '%foo bar%'))",
        &[],
    ),
    (
        "title:",
        "(tsvector_combined @@ to_tsquery('', 'title:'))",
        &["title:"],
    ),
    ("author:bar", "( (LOWER(author) LIKE '%bar%'))", &[]),
    ("note:true", "( (note IS NOT NULL AND note != ''))", &[]),
    ("note:false", "( (note IS NULL OR note = ''))", &[]),
    (
        "note:abc",
        "( (LOWER(COALESCE(note, '')) LIKE '%abc%'))",
        &[],
    ),
    ("star:true", "( (marked = true))", &[]),
    ("-star:true", "(NOT (marked = true))", &[]),
    ("star:false", "( (marked = false))", &[]),
    (
        "star:maybe",
        "(tsvector_combined @@ to_tsquery('', 'star:maybe'))",
        &["star:maybe"],
    ),
    ("pub:true", "( (published = true))", &[]),
    ("pub:false", "( (published = false))", &[]),
    ("unread:true", "( (unread = true))", &[]),
    ("-unread:false", "(NOT (unread = false))", &[]),
    (
        "label:true",
        "(ttrss_entries.id  IN (SELECT article_id FROM ttrss_user_labels2))",
        &[],
    ),
    (
        "label:false",
        "(NOT (ttrss_entries.id  IN (SELECT article_id FROM ttrss_user_labels2)))",
        &[],
    ),
    (
        "label:Inbox",
        "(ttrss_entries.id  IN (SELECT article_id FROM ttrss_user_labels2 WHERE label_id = 4))",
        &[],
    ),
    ("label:DoesNotExist", "(false)", &[]),
    (
        "-label:Inbox",
        "(ttrss_entries.id NOT IN (SELECT article_id FROM ttrss_user_labels2 WHERE label_id = 4))",
        &[],
    ),
    (
        "tag:true",
        "(ttrss_user_entries.int_id  IN (SELECT post_int_id FROM ttrss_tags))",
        &[],
    ),
    (
        "tag:false",
        "(NOT (ttrss_user_entries.int_id  IN (SELECT post_int_id FROM ttrss_tags)))",
        &[],
    ),
    (
        "tag:foo",
        "(ttrss_user_entries.int_id  IN (SELECT post_int_id FROM ttrss_tags WHERE tag_name = 'foo'))",
        &[],
    ),
    (
        "-tag:bar",
        "(ttrss_user_entries.int_id NOT IN (SELECT post_int_id FROM ttrss_tags WHERE tag_name = 'bar'))",
        &[],
    ),
    (
        "unknown:foo",
        "(tsvector_combined @@ to_tsquery('', 'unknown:foo'))",
        &["unknown:foo"],
    ),
    (
        "@date:2020-01-15",
        "( will generate an error )",
        &["The date keyword \"date:2020-01-15\" was not recognized."],
    ),
    (
        "@date:\"last Monday\"",
        "( will generate an error )",
        &["The date keyword \"date:last monday\" was not recognized."],
    ),
    (
        "@bogus-word",
        "( will generate an error )",
        &["The date keyword \"bogus-word\" was not recognized."],
    ),
    (
        "@2020-01-15",
        "(  (updated >= '2020-01-15 00:00:00' AND updated <= '2020-01-15 23:59:59'))",
        &[],
    ),
    (
        "title:foo -star:true bar",
        "( (LOWER(ttrss_entries.title) LIKE '%foo%')) AND (NOT (marked = true)) AND (tsvector_combined @@ to_tsquery('', 'bar'))",
        &["bar"],
    ),
    (
        "hello \"quoted phrase\" world",
        "(tsvector_combined @@ to_tsquery('', 'hello (quoted <-> phrase) world'))",
        &["hello", "quoted phrase", "world"],
    ),
    (
        "-a -b -c",
        "(tsvector_combined @@ to_tsquery('', '!a !b !c'))",
        &[],
    ),
    (
        "a b c d",
        "(tsvector_combined @@ to_tsquery('', 'a & b & c & d'))",
        &["a", "b", "c", "d"],
    ),
    (
        "title:it's",
        "( (LOWER(ttrss_entries.title) LIKE '%it''s%'))",
        &[],
    ),
    (
        "tag:it's",
        "(ttrss_user_entries.int_id  IN (SELECT post_int_id FROM ttrss_tags WHERE tag_name = 'it''s'))",
        &[],
    ),
    (
        "hello world | title:foo",
        "( (LOWER(ttrss_entries.title) LIKE '%foo%')) AND (tsvector_combined @@ to_tsquery('', 'hello world |'))",
        &["hello", "world"],
    ),
    (
        "\"a b\" & c",
        "(tsvector_combined @@ to_tsquery('', '(a <-> b) & c'))",
        &["a b", "c"],
    ),
    (
        "star:true title:\"x y\" -unread:true",
        "( (marked = true)) AND ( (LOWER(ttrss_entries.title) LIKE '%x y%')) AND (NOT (unread = true))",
        &[],
    ),
    (
        "-hello | world",
        "(tsvector_combined @@ to_tsquery('', '!hello | world'))",
        &["world"],
    ),
    ("!", "(tsvector_combined @@ to_tsquery('', '!'))", &[]),
    ("(", "(tsvector_combined @@ to_tsquery('', '('))", &[]),
    (
        "a!b",
        "(tsvector_combined @@ to_tsquery('', 'a!b'))",
        &["a!b"],
    ),
    (
        "a & b | !c",
        "(tsvector_combined @@ to_tsquery('', 'a & b | !c'))",
        &["a", "b", "!c"],
    ),
    (
        "\"a b\" \"c d\"",
        "(tsvector_combined @@ to_tsquery('', '(a <-> b) (c <-> d)'))",
        &["a b", "c d"],
    ),
    (
        "title:\"\" x",
        "( (LOWER(ttrss_entries.title) LIKE '%\"\"%')) AND (tsvector_combined @@ to_tsquery('', 'x'))",
        &["x"],
    ),
    (
        "unread: maybe",
        "(tsvector_combined @@ to_tsquery('', 'unread: & maybe'))",
        &["unread:", "maybe"],
    ),
    (
        "title:  spaced  ",
        "(tsvector_combined @@ to_tsquery('', 'title: & spaced'))",
        &["title:", "spaced"],
    ),
    (
        "-\"a b\" c",
        "(tsvector_combined @@ to_tsquery('', '!(a <-> b) c'))",
        &["c"],
    ),
    (
        "@date:yesterday",
        "( will generate an error )",
        &["The date keyword \"date:yesterday\" was not recognized."],
    ),
];

/// (input, date kind, expected query_part template; `<<D>>` = the day)
const DATE_CASES: &[(&str, &str, &str)] = &[
    (
        "@yesterday",
        "yesterday",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "@today",
        "today",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "@\"last Monday\"",
        "last_monday",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "-@\"last Monday\"",
        "last_monday",
        "( NOT (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "@now",
        "today",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "@tomorrow",
        "tomorrow",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59'))",
    ),
    (
        "@yesterday title:foo",
        "yesterday",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59')) AND ( (LOWER(ttrss_entries.title) LIKE '%foo%'))",
    ),
    (
        "@  x",
        "today",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59')) AND (tsvector_combined @@ to_tsquery('', 'x'))",
    ),
    (
        "x @  y",
        "today",
        "(  (updated >= '<<D>> 00:00:00' AND updated <= '<<D>> 23:59:59')) AND (tsvector_combined @@ to_tsquery('', 'x & y'))",
    ),
];

#[tokio::test]
async fn search_to_sql_fragments() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");

            // Match the oracle DB's label set (labels 1-2 come from the fixture).
            sqlx::query(
                "INSERT INTO ttrss_labels2 (id, owner_uid, caption) VALUES
         (3, 1, 'Project X'), (4, 1, 'Inbox'), (5, 1, 'Archive'),
         (6, 1, 'Someday'), (7, 1, 'Work'), (8, 1, 'Personal')",
            )
            .execute(db.pool())
            .await
            .expect("seed labels");

            for (input, want_part, want_words) in CASES {
                let parts = search::search_to_sql(&db, 1, None, input, "").await;
                assert_eq!(parts.query_part, *want_part, "input: {input:?}");
                assert_eq!(parts.words, *want_words, "input: {input:?}");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn search_to_sql_date_cases() {
    with_test_db(|url| {
        Box::pin(async move {
            let db = common::make_db_from_url(&url).await.expect("test db");

            let today = chrono::Utc::now().date_naive();
            let wd = today.weekday().num_days_from_monday() as i64;
            let dates: std::collections::HashMap<&str, chrono::NaiveDate> = [
                ("today", today),
                ("yesterday", today - chrono::Duration::days(1)),
                ("tomorrow", today + chrono::Duration::days(1)),
                // PHP "last <weekday>": the previous occurrence (7 days back when
                // today already is that weekday).
                (
                    "last_monday",
                    today - chrono::Duration::days(if wd == 0 { 7 } else { wd }),
                ),
            ]
            .into_iter()
            .collect();

            for (input, kind, template) in DATE_CASES {
                let want = template.replace("<<D>>", &dates[kind].to_string());
                let parts = search::search_to_sql(&db, 1, None, input, "").await;
                assert_eq!(parts.query_part, want, "input: {input:?}");
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}
