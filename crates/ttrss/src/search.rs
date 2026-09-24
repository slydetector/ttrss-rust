//! Port of `Feeds::_search_to_sql` (classes/Feeds.php) — the tt-rss search
//! query → SQL fragment converter, shared by the headline list and
//! `Feeds::_catchup`.
//!
//! Query language (per the PHP docblock):
//! - `key:value` pairs: `title:`, `author:`, `note:`, `star:`, `pub:`,
//!   `unread:`, `label:`, `tag:`
//! - `@date` values (PHP `DateTime` parsing — see [`crate::search_date`])
//! - PostgreSQL FTS: plain words, `"consecutive words"` (`<->`), tsquery
//!   operators `& | !` and parentheses (partially supported, as in PHP)
//! - `-keyword` negates a keyword
//!
//! Unsupported `key:value` / `@date` values fall back to FTS exactly like
//! PHP (invalid FTS then surfaces as "Incorrect search syntax" in the
//! headline dry-run, or a runtime error in `_catchup`).

use regex::Regex;
use std::sync::LazyLock;
use sqlx::AssertSqlSafe;

use crate::core::db::Db;

use crate::labels;
use crate::prefs;
use crate::search_date;

/// Result of [`search_to_sql`] (PHP's `[$search_query_part, $search_words]`).
pub struct SearchParts {
    /// SQL fragment for the WHERE clause (`"false"` when nothing matched).
    pub query_part: String,
    /// Highlight words (also the tail of the "Incorrect search syntax"
    /// message on a failed dry-run).
    pub words: Vec<String>,
}

/// `Feeds::_search_to_sql($search, $search_language, $owner_uid, $profile)`
/// (loads the owner's prefs rows).
pub async fn search_to_sql(
    db: &Db,
    owner_uid: i64,
    profile: Option<i32>,
    search: &str,
    search_language: &str,
) -> SearchParts {
    let rows = prefs::load_rows(db, owner_uid, profile).await;
    search_to_sql_cached(
        db.pool(),
        &rows,
        owner_uid,
        profile,
        search,
        search_language,
    )
    .await
}

/// The same conversion over an already-loaded prefs row set (callers that
/// already hold `rows` — the headline list and `_catchup` — avoid a second
/// prefs fetch).
pub async fn search_to_sql_cached(
    pool: &sqlx::PgPool,
    rows: &[(String, Option<i32>, String)],
    owner_uid: i64,
    profile: Option<i32>,
    search: &str,
    search_language: &str,
) -> SearchParts {
    // PHP: the date branch reads USER_TIMEZONE without a profile.
    let user_tz = prefs::resolve(prefs::USER_TIMEZONE, rows, None)
        .as_str()
        .to_string();

    // Quote grouping so command pairs tokenize as one keyword:
    //   keyword:"foo bar" -> "keyword:foo bar"
    //   -"hello world"    -> "-hello world"
    //   @"last Tuesday"   -> "@last Tuesday"
    let search_csv_str = preprocess(search);

    let keywords = str_getcsv_space(&search_csv_str);

    let mut query_keywords: Vec<String> = Vec::new();
    let mut search_words: Vec<String> = Vec::new();
    let mut leftover: Vec<String> = Vec::new();

    for k_raw in keywords {
        // PHP `null` keyword (empty input) coerces to "".
        let mut k: String = k_raw;
        let not = if let Some(rest) = k.strip_prefix('-') {
            k = rest.to_string();
            // PHP: $not = 'NOT' (no trailing space; templates add their own).
            "NOT"
        } else {
            ""
        };

        // Only a left trim — spaces in the value part of keyword pairs are
        // kept (needed for precise title/author/note searches).
        let k = k.trim_start().to_lowercase();

        let mut valid_keyword_processed = false;

        // ---- key:value pairs -------------------------------------------
        if let Some((name, value_untrimmed)) = k.split_once(':').filter(|(_, v)| !v.is_empty()) {
            let value = value_untrimmed.trim();
            match name {
                "title" => {
                    query_keywords.push(format!(
                        "({not} (LOWER(ttrss_entries.title) LIKE {}))",
                        pdo_quote(&format!("%{value_untrimmed}%"))
                    ));
                    valid_keyword_processed = true;
                }
                "author" => {
                    query_keywords.push(format!(
                        "({not} (LOWER(author) LIKE {}))",
                        pdo_quote(&format!("%{value_untrimmed}%"))
                    ));
                    valid_keyword_processed = true;
                }
                "note" => {
                    if value == "true" {
                        query_keywords.push(format!("({not} (note IS NOT NULL AND note != ''))"));
                    } else if value == "false" {
                        query_keywords.push(format!("({not} (note IS NULL OR note = ''))"));
                    } else {
                        query_keywords.push(format!(
                            "({not} (LOWER(COALESCE(note, '')) LIKE {}))",
                            pdo_quote(&format!("%{value_untrimmed}%"))
                        ));
                    }
                    valid_keyword_processed = true;
                }
                "star" => match value {
                    "true" => {
                        query_keywords.push(format!("({not} (marked = true))"));
                        valid_keyword_processed = true;
                    }
                    "false" => {
                        query_keywords.push(format!("({not} (marked = false))"));
                        valid_keyword_processed = true;
                    }
                    // invalid -> fall back to FTS (the dry-run then fails)
                    _ => {}
                },
                "pub" => match value {
                    "true" => {
                        query_keywords.push(format!("({not} (published = true))"));
                        valid_keyword_processed = true;
                    }
                    "false" => {
                        query_keywords.push(format!("({not} (published = false))"));
                        valid_keyword_processed = true;
                    }
                    _ => {}
                },
                "unread" => match value {
                    "true" => {
                        query_keywords.push(format!("({not} (unread = true))"));
                        valid_keyword_processed = true;
                    }
                    "false" => {
                        query_keywords.push(format!("({not} (unread = false))"));
                        valid_keyword_processed = true;
                    }
                    _ => {}
                },
                "label" => {
                    let sql_start = format!(
                        "(ttrss_entries.id {not} IN (SELECT article_id FROM ttrss_user_labels2"
                    );
                    let sql_end = "))";
                    if value == "true" {
                        query_keywords.push(format!("{sql_start}{sql_end}"));
                    } else if value == "false" {
                        query_keywords.push(format!("(NOT {sql_start}{sql_end})"));
                    } else {
                        let label_id = labels::find_id(pool, value, owner_uid).await;
                        if label_id != 0 {
                            query_keywords
                                .push(format!("{sql_start} WHERE label_id = {label_id}{sql_end}"));
                        } else {
                            query_keywords.push(if not.is_empty() {
                                "(false)".to_string()
                            } else {
                                "(true)".to_string()
                            });
                        }
                    }
                    valid_keyword_processed = true;
                }
                "tag" => {
                    let sql_start = format!(
                        "(ttrss_user_entries.int_id {not} IN (SELECT post_int_id FROM ttrss_tags"
                    );
                    let sql_end = "))";
                    if value == "true" {
                        query_keywords.push(format!("{sql_start}{sql_end}"));
                    } else if value == "false" {
                        query_keywords.push(format!("(NOT {sql_start}{sql_end})"));
                    } else {
                        query_keywords.push(format!(
                            "{sql_start} WHERE tag_name = {}{sql_end}",
                            pdo_quote(value)
                        ));
                    }
                    valid_keyword_processed = true;
                }
                // unknown key -> fall back to FTS
                _ => {}
            }
        }

        // ---- @date keywords --------------------------------------------
        if !valid_keyword_processed && k.starts_with('@') {
            let query_date = k.trim_start_matches('@').trim().to_string();
            match search_date::php_date_day(&query_date, &user_tz) {
                Some(day) => {
                    let (first, last) = search_date::day_range_utc(day, &user_tz);
                    query_keywords.push(format!(
                        "( {not} (updated >= {} AND updated <= {}))",
                        pdo_quote(&first),
                        pdo_quote(&last)
                    ));
                }
                // PHP catch: fake a wrong-SQL fragment so the dry-run fails
                // with a warning.
                None => {
                    query_keywords.push("( will generate an error )".to_string());
                    search_words.push(format!(
                        "The date keyword \"{}\" was not recognized.",
                        php_htmlspecialchars(&query_date)
                    ));
                }
            }
            valid_keyword_processed = true;
        }

        // ---- full text search ------------------------------------------
        if !valid_keyword_processed {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            let k_sql = if k.contains(char::is_whitespace) {
                format!("({})", RE_WS.replace_all(k, " <-> "))
            } else {
                k.to_string()
            };
            leftover.push(if not.is_empty() {
                k_sql
            } else {
                format!("!{k_sql}")
            });

            if not.is_empty() && !RE_BARE_OP.is_match(k) {
                search_words.push(k.to_string());
            }
        }
    }

    if !leftover.is_empty() {
        // No logical operators: a "simple" search — AND everything.
        // Otherwise trust the user's tsquery syntax as-is.
        let concatenated = leftover.join(" ");
        let tsquery = if RE_FTS_OP.is_match(&concatenated) {
            concatenated
        } else {
            leftover.join(" & ")
        };

        // `$search_language ?: Prefs::get(DEFAULT_SEARCH_LANGUAGE, …)` —
        // PHP truthiness: "" and "0" are falsy.
        let lang = if search_language.is_empty() || search_language == "0" {
            prefs::resolve(prefs::DEFAULT_SEARCH_LANGUAGE, rows, profile)
                .as_str()
                .to_string()
        } else {
            search_language.to_string()
        };

        query_keywords.push(format!(
            "(tsvector_combined @@ to_tsquery({}, {}))",
            pdo_quote(&lang.to_lowercase()),
            pdo_quote(&tsquery)
        ));
    }

    SearchParts {
        query_part: if query_keywords.is_empty() {
            "false".to_string()
        } else {
            query_keywords.join(" AND ")
        },
        words: search_words,
    }
}

/// Quote grouping so command pairs survive space-delimited tokenization:
/// `keyword:"foo bar"` -> `"keyword:foo bar"`, `-"hello world"` -> `-"hello
/// world"`, `@"last Tuesday"` -> `@"last Tuesday"`.
pub(crate) fn preprocess(search: &str) -> String {
    let s = RE_KV_QUOTE
        .replace_all(search.trim(), "\"$1:$2\"")
        .into_owned();
    let s = RE_NEG_QUOTE.replace_all(&s, "\"-$1\"").into_owned();
    RE_AT_QUOTE.replace_all(&s, "\"$1$2\"").into_owned()
}

/// The headline dry-run: `select <qpart> FROM ttrss_entries,
/// ttrss_user_entries WHERE id = ref_id limit 1`. Returns the (possibly
/// replaced) query part plus the "Incorrect search syntax" message when the
/// fragment is not executable.
pub async fn dry_run(
    pool: &sqlx::PgPool,
    query_part: &str,
    words: &[String],
) -> (String, String) {
    let sql = format!(
        "select {query_part}
\t\t\t\tFROM ttrss_entries, ttrss_user_entries WHERE id = ref_id limit 1"
    );
    match sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await {
        Ok(_) => (query_part.to_string(), String::new()),
        Err(_) => (
            "false".to_string(),
            crate::i18n::t_sprintf("Incorrect search syntax: %s.", &[
                &words.join(" "),
            ]),
        ),
    }
}

/// `PDO::quote` (pgsql driver): single quotes doubled, backslashes untouched,
/// the string truncated at the first NUL, wrapped in single quotes.
fn pdo_quote(s: &str) -> String {
    let end = s.find('\0').unwrap_or(s.len());
    let s = &s[..end];
    let mut out = String::with_capacity(s.len() + 2 + s.matches('\'').count());
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// `htmlspecialchars($s)` (default `ENT_QUOTES | ENT_SUBSTITUTE`, UTF-8).
fn php_htmlspecialchars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            _ => out.push(ch),
        }
    }
    out
}

/// `str_getcsv($s, ' ', '"', '')` — the PHP 8 state machine (enclosure only
/// takes effect at a field start; an enclosure inside an enclosed field
/// closes the field only when followed by the delimiter or EOF, doubles to a
/// literal, or drops into plain state otherwise).
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Strip a trailing `\n`, `\r`, or `\r\n` (PHP
/// `php_fgetcsv_lookup_trailing_spaces` on a field buffer).
fn strip_cr_lf(s: &str) -> &str {
    // Exact port of `php_fgetcsv_lookup_trailing_spaces` applied to a
    // finished field: only the final one or two bytes matter (a UTF-8
    // continuation/lead byte can never be CR or LF).
    let b = s.as_bytes();
    if let Some(&last) = b.last() {
        if last == b'\n' {
            if b.len() > 1 && b[b.len() - 2] == b'\r' {
                return &s[..b.len() - 2];
            }
            return &s[..b.len() - 1];
        }
        if last == b'\r' {
            return &s[..b.len() - 1];
        }
    }
    s
}

/// `str_getcsv($s, ' ', '"', '')` — a faithful port of PHP 8.5's
/// `php_fgetcsv` (ext/standard/file.c) with an empty escape character:
/// enclosure only applies at a field start; a quote inside an enclosed field
/// is doubled into a literal, or (when followed by anything else) ends the
/// enclosed portion and the field continues in plain state to the delimiter,
/// with the dangling quote dropped. Verified against the live PHP 8.5 oracle.
fn str_getcsv_space(s: &str) -> Vec<String> {
    const DELIM: u8 = b' ';
    const ENC: u8 = b'"';
    let bytes = s.as_bytes();
    let len = bytes.len();

    // `limit`: trailing CR/LF are set aside (added back to an unterminated
    // enclosed field). For inputs without a line ending, limit == len.
    let mut limit = len;
    if limit > 0 && bytes[limit - 1] == b'\n' {
        limit -= 1;
        if limit > 0 && bytes[limit - 1] == b'\r' {
            limit -= 1;
        }
    } else if limit > 0 && bytes[limit - 1] == b'\r' {
        limit -= 1;
    }

    let mut fields: Vec<String> = Vec::new();
    let mut bptr = 0usize;
    let mut first = true;

    loop {
        let inc = bptr < limit;
        if inc {
            // skip leading whitespace for the purpose of finding an
            // opening enclosure (the field itself still starts at bptr)
            let mut tmp = bptr;
            while tmp < limit && bytes[tmp] != DELIM && is_space(bytes[tmp]) {
                tmp += 1;
            }
            if tmp < limit && bytes[tmp] == ENC {
                bptr = tmp;
            }
        }

        // PHP: a blank line yields NULL -> [null], which downstream behaves
        // like a single empty keyword.
        if first && bptr == limit {
            return vec![String::new()];
        }
        first = false;

        let delim_found;
        let mut field = String::new();

        if inc && bytes[bptr] == ENC {
            // ---- 2A: enclosure-delimited field ----
            let mut state = 0;
            bptr += 1;
            let mut hunk = bptr;
            loop {
                if bptr >= limit {
                    if state == 2 {
                        // pending quote at end of data: drop it
                        field.push_str(&s[hunk..bptr - 1]);
                        hunk = bptr;
                    } else {
                        if hunk != limit {
                            field.push_str(&s[hunk..bptr]);
                            hunk = bptr;
                        }
                        // add the embedded line end back to the field
                        field.push_str(&s[limit..len]);
                    }
                    // (stream == NULL: the enclosure is unterminated)
                    while bptr < limit && bytes[bptr] != DELIM {
                        bptr += 1;
                    }
                    delim_found = bptr < limit;
                    field.push_str(&s[hunk..bptr]);
                    if delim_found {
                        bptr += 1;
                    }
                    break;
                }
                let c = bytes[bptr];
                if state == 2 {
                    if c != ENC {
                        // real enclosure: drop the quote, the field then
                        // continues in plain state up to the delimiter
                        field.push_str(&s[hunk..bptr - 1]);
                        hunk = bptr;
                        while bptr < limit && bytes[bptr] != DELIM {
                            bptr += 1;
                        }
                        delim_found = bptr < limit;
                        field.push_str(&s[hunk..bptr]);
                        if delim_found {
                            bptr += 1;
                        }
                        break;
                    }
                    // doubled quote -> literal
                    field.push_str(&s[hunk..bptr]);
                    bptr += 1;
                    hunk = bptr;
                    state = 0;
                } else {
                    if c == ENC {
                        state = 2;
                    }
                    bptr += 1;
                }
            }
        } else {
            // ---- 2B: non-enclosure field ----
            let hunk = bptr;
            while bptr < limit && bytes[bptr] != DELIM {
                bptr += 1;
            }
            delim_found = bptr < limit;
            field = strip_cr_lf(&s[hunk..bptr]).to_string();
            if delim_found {
                bptr += 1;
            }
        }

        fields.push(field);
        if !delim_found {
            break;
        }
    }

    fields
}

// `(-?\w+):"([^"]+?)"` — PHP PCRE `\w` is ASCII [a-zA-Z0-9_] here.
// `(-?\w+):"([^"]+?)"` -> `"$1:$2"` (the match is re-wrapped so the whole
// `key:value` pair becomes one quoted field)
static RE_KV_QUOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r###"(-?[a-zA-Z0-9_]+):"([^"]+?)""###).unwrap());
// `-"([^"]+?)"`
static RE_NEG_QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r###"-"([^"]+?)""###).unwrap());
// `(-?\@)"([^"]+?)"`
static RE_AT_QUOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r###"(-?@)"([^"]+?)""###).unwrap());
// `\s+` (PHP: any whitespace run)
static RE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
// `[&|!()]`
static RE_FTS_OP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[&|!()]").unwrap());
// `^[&|!()]$`
static RE_BARE_OP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[&|!()]$").unwrap());

#[cfg(test)]
mod tests {
    use super::*;

    // ---- str_getcsv space-delimited, oracle-verified (PHP 8.5) ----------
    // 142 vectors diffed against PHP 8.5 `str_getcsv($s, ' ', '"', '')`:
    // a byte-exact C port of php_fgetcsv (8.5) and the live oracle both
    // reproduce this table. PHP's `[null]` for empty input is modelled as
    // `[""]` (identical behavior downstream in the keyword loop).
    const TOKENIZER_CASES: &[(&str, &[&str])] = &[
        ("", &[""]),
        ("a", &["a"]),
        ("a ", &["a", ""]),
        (" a", &["", "a"]),
        ("  ", &["", "", ""]),
        ("   ", &["", "", "", ""]),
        ("a b c", &["a", "b", "c"]),
        ("-a -b", &["-a", "-b"]),
        ("-a:1", &["-a:1"]),
        ("-a:b", &["-a:b"]),
        ("-a:b:", &["-a:b:"]),
        ("\"-\"", &["-"]),
        ("\"-a\" b", &["-a", "b"]),
        ("\"a b\" c", &["a b", "c"]),
        ("\"a b\"\" c", &["a b\" c"]),
        ("\"a\"\" \"b\"", &["a\" b\""]),
        ("\"a b\" \"\"", &["a b", ""]),
        ("\"a\" \"\"", &["a", ""]),
        ("\"a\"b", &["ab"]),
        ("\"a\"\"b\"", &["a\"b"]),
        ("\"a\" \"\"b\"", &["a", "b\""]),
        ("\"a b\"\" \"c d\"", &["a b\" c", "d\""]),
        ("\"a\"\"b\"\" c", &["a\"b\" c"]),
        ("\"a b\"\" c\"\"", &["a b\" c\""]),
        ("\"a\" \"b\" c", &["a", "b", "c"]),
        ("\"\"", &[""]),
        ("\"\\\"\"", &["\\\""]),
        ("\"a b\"\"", &["a b\""]),
        ("\"a b\" c\"\"", &["a b", "c\"\""]),
        ("\"a b\" c\" d\"\"", &["a b", "c\"", "d\"\""]),
        ("\"a\"b\" \"c\" d\"", &["ab\"", "c", "d\""]),
        ("\"a\"b\" c\" \"", &["ab\"", "c\"", ""]),
        ("\"a\" b\"\"", &["a", "b\"\""]),
        ("\"\"\"c\"\"", &["\"c\""]),
        ("\"\"\" c\"\"", &["\" c\""]),
        ("\"a\" \"\"", &["a", ""]),
        ("-\"a\"b c", &["-\"a\"b", "c"]),
        ("\"-\"x", &["-x"]),
        ("x \"a\"b y", &["x", "ab", "y"]),
        ("\"a\"\" ", &["a\" "]),
        ("a b \"c d\"", &["a", "b", "c d"]),
        ("a\"b ", &["a\"b", ""]),
        ("\"a\"\" \"", &["a\" "]),
        ("\"a b\"\"c\"", &["a b\"c"]),
        ("\"x\"y z\"w\"", &["xy", "z\"w\""]),
        ("\"a\" b\"c\" d", &["a", "b\"c\"", "d"]),
        ("\"a\" c\"", &["a", "c\""]),
        ("\"q\"r s\" t\"", &["qr", "s\"", "t\""]),
        ("\"a\"b\" c\" d\" e\"", &["ab\"", "c\"", "d\"", "e\""]),
        ("\"a\"b\" c\"d\"", &["ab\"", "c\"d\""]),
        ("\"a\"b\" c\"\"", &["ab\"", "c\"\""]),
        ("\"a\"b\" \"c\"", &["ab\"", "c"]),
        ("\"a\"b\" c\" d\"", &["ab\"", "c\"", "d\""]),
        ("\"a\"\" \"b\" c\"", &["a\" b\"", "c\""]),
        ("\"a\"b\"c", &["ab\"c"]),
        ("\"a\"b c", &["ab", "c"]),
        ("\"a\"b\"", &["ab\""]),
        ("\"a\" \"b\"", &["a", "b"]),
        ("\"abc", &["abc"]),
        ("\"", &[""]),
        ("\"\"a\"", &["a\""]),
        ("\"a\" ", &["a", ""]),
        ("\"a\"  ", &["a", "", ""]),
        ("a  b", &["a", "", "b"]),
        ("a\tx", &["a\tx"]),
        ("\"a\"b\t", &["ab\t"]),
        ("a \t", &["a", "\t"]),
        ("a\t", &["a\t"]),
        ("\"a\"\t ", &["a\t", ""]),
        ("tab\there x", &["tab\there", "x"]),
        ("\"a\nc\" d", &["a\nc", "d"]),
        ("a\nb", &["a\nb"]),
        ("\"a\"b\"\tc\" d\"", &["ab\"\tc\"", "d\""]),
        ("  \"a\"  b", &["", "", "a", "", "b"]),
        ("\" a\" b", &[" a", "b"]),
        (" \"a\" b", &["", "a", "b"]),
        ("\" a b\" c", &[" a b", "c"]),
        ("\"a  b\"", &["a  b"]),
        ("a b  c", &["a", "b", "", "c"]),
        ("\"a b\"  c", &["a b", "", "c"]),
        ("\"a\" \"b\" \"c\"", &["a", "b", "c"]),
        ("\"a\"b\"c\"d", &["ab\"c\"d"]),
        ("\"a\"\"b\"\"c\"", &["a\"b\"c"]),
        ("\"a\"\"\" b\"", &["a\"", "b\""]),
        ("\"a b\"\"\" c\"", &["a b\"", "c\""]),
        ("\"a b\" c\"\"\" d\"", &["a b", "c\"\"\"", "d\""]),
        ("\"\"\"\"\"", &["\"\""]),
        ("\"a\"\"\"\"", &["a\"\""]),
        ("\"a\"\"\"\" b\"", &["a\"\" b"]),
        ("\"a b c\" d\" e\"", &["a b c", "d\"", "e\""]),
        ("\"a\"b\"\" c\" d\" e\"", &["ab\"\"", "c\"", "d\"", "e\""]),
        ("a\"b\"c\"d", &["a\"b\"c\"d"]),
        ("\"a\" \"b\" \"c\" \"d\"", &["a", "b", "c", "d"]),
        ("\"a\"\"\"\"\" b\"", &["a\"\"", "b\""]),
        ("\"a b\" \"c d\" \"e f\"", &["a b", "c d", "e f"]),
        ("\"a b\"\" \"c d\"", &["a b\" c", "d\""]),
        ("\"a b\" \"c d\"\"", &["a b", "c d\""]),
        ("x y\" z", &["x", "y\"", "z"]),
        ("x\" y z", &["x\"", "y", "z"]),
        ("\"x y\" z\"", &["x y", "z\""]),
        ("\"x\"\" y z\"", &["x\" y z"]),
        ("\"x y\"\" z\"", &["x y\" z"]),
        ("a\" b\" c", &["a\"", "b\"", "c"]),
        ("\"a\"b\"c d\"e\"", &["ab\"c", "d\"e\""]),
        ("\"a\"\"b c\" d\"\"e\"", &["a\"b c", "d\"\"e\""]),
        ("  a  b  ", &["", "", "a", "", "b", "", ""]),
        ("a \", b", &["a", ", b"]),
        ("a , b", &["a", ",", "b"]),
        ("\"a,b\" c", &["a,b", "c"]),
        ("\"a ,b\" c", &["a ,b", "c"]),
        ("a\t\"b\"\tc", &["a\t\"b\"\tc"]),
        ("\"a\"\t\"b\"", &["a\t\"b\""]),
        ("\"a\" \t\"b\"", &["a", "b"]),
        ("\t\"a\"", &["a"]),
        ("\"a\"\t", &["a\t"]),
        ("a\tb\tc", &["a\tb\tc"]),
        ("\"a\"b\tc\"d\"", &["ab\tc\"d\""]),
        ("\"a\"b\t\"c\"", &["ab\t\"c\""]),
        ("\"a b\"\t\"c d\"", &["a b\t\"c", "d\""]),
        ("\r\"a\"", &["a"]),
        ("\"a\"\r", &["a"]),
        ("a\rb c", &["a\rb", "c"]),
        ("\"a b\"\r c\"", &["a b\r", "c\""]),
        ("\"a\" b\r", &["a", "b"]),
        ("\"a\r\" b\"", &["a\r", "b\""]),
        ("\"a\" \"b\"c \"d\"", &["a", "bc", "d"]),
        ("\"a\"\" \"b\"\" c\"", &["a\" b\"\"", "c\""]),
        ("\"\"\"\"\"\" \"a\"", &["\"\"", "a"]),
        ("\"a\"\"\"\"\"\" \"b\"", &["a\"\"\" b\""]),
        ("\"a b\"\"\"\"\"\" c\"", &["a b\"\"\" c"]),
        ("\"a\"\"\"\"\" b c\"", &["a\"\"", "b", "c\""]),
        ("\"a b c d\" e f g\"", &["a b c d", "e", "f", "g\""]),
        ("\"a\"\"b\"\" c\"\"d\" e\"", &["a\"b\" c\"d", "e\""]),
        (
            "\"a\"\"b\"\"c\" d\" e\"\"f\"",
            &["a\"b\"c", "d\"", "e\"\"f\""],
        ),
        ("\"a b\"\"\" c\"\" d\"", &["a b\"", "c\"\"", "d\""]),
        ("\"a\"b\"\"\" c\"", &["ab\"\"\"", "c\""]),
        (
            "\"a\"\"b\"\" c\" d\" e\" f\"",
            &["a\"b\" c", "d\"", "e\"", "f\""],
        ),
        ("\"a\"\"\"\"b\"\"\"\"c\"", &["a\"\"b\"\"c"]),
        ("\"a b\"\" c\"\" d\"", &["a b\" c\" d"]),
        ("\"a b c\"\" d e\" f\"", &["a b c\" d e", "f\""]),
        ("\"a\"b c\"d e\"f g\"", &["ab", "c\"d", "e\"f", "g\""]),
        (
            "\"a\"b\" c\"d\" e\"f\" g\"",
            &["ab\"", "c\"d\"", "e\"f\"", "g\""],
        ),
    ];

    #[test]
    fn tokenizer_matches_php() {
        for (input, want) in TOKENIZER_CASES {
            let got = str_getcsv_space(input);
            assert_eq!(got, *want, "input: {input:?}");
        }
    }

    // ---- quote grouping --------------------------------------------------
    #[test]
    fn preprocess_quote_grouping() {
        // Oracle-verified: exact `preg_replace` sequence from `_search_to_sql`
        // (Feeds.php 2097-2099) run on the live PHP 8.5 oracle.
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("   ", ""),
            ("title:\"a b\" c", "\"title:a b\" c"),
            ("title:\"a b\" c d", "\"title:a b\" c d"),
            ("-a \"b c\"", "-a \"b c\""),
            ("-title:a b\"", "-title:a b\""),
            ("a:1 b:2", "a:1 b:2"),
            ("a:1 \"b c\"", "a:1 \"b c\""),
            ("\"-a b\" c", "\"-a b\" c"),
            ("-a:1 \"b c\"", "-a:1 \"b c\""),
            ("@\"a b\"", "\"@a b\""),
            ("-\"a b\"", "\"-a b\""),
            ("-a \"@b c\"", "-a \"@b c\""),
            ("a \"b c\" d", "a \"b c\" d"),
            ("title:\"\" x", "title:\"\" x"),
            ("title:\"", "title:\""),
            ("\"title\" x", "\"title\" x"),
            ("key:\"a:b\" c", "\"key:a:b\" c"),
            ("-key:\"a:b\" c", "\"-key:a:b\" c"),
            ("a-\"b c\" d", "a\"-b c\" d"),
            ("x-\"y z\"", "x\"-y z\""),
            ("@date:\"yesterday\" x", "\"@date:yesterday\" x"),
            ("foo@bar \"b c\"", "foo@bar \"b c\""),
            ("-@\"b c\"", "\"-@b c\""),
            ("a:1 \"b c\" d:2", "a:1 \"b c\" d:2"),
            ("k:\"v\" k:\"w\"", "\"k:v\" \"k:w\""),
            ("\"a\" \"b\" \"c\"", "\"a\" \"b\" \"c\""),
            ("title:\"a b\" c:\"d e\"", "\"title:a b\" \"c:d e\""),
            ("-title:\"a b\" -x:\"c d\"", "\"-title:a b\" \"-x:c d\""),
            ("@date:yesterday @time:today", "@date:yesterday @time:today"),
            ("plain words here", "plain words here"),
            ("  leading and trailing  ", "leading and trailing"),
            ("a  double  space", "a  double  space"),
            ("tab\there and \"quoted\" x", "tab\there and \"quoted\" x"),
            ("new\nline here", "new\nline here"),
            ("a\"b c\"d e", "a\"b c\"d e"),
            ("\"unclosed a b", "\"unclosed a b"),
            ("a \"b c\" d \"e f\"", "a \"b c\" d \"e f\""),
            ("-x \"a b\" y \"c d\" z", "-x \"a b\" y \"c d\" z"),
            ("w:\"1 2\" -y:\"3 4\" z", "\"w:1 2\" \"-y:3 4\" z"),
            ("@\"@\" q", "\"@@\" q"),
            (
                "title:plain -tag:\"has space\"",
                "title:plain \"-tag:has space\"",
            ),
            ("a-\"b\" c", "a\"-b\" c"),
            ("--\"a b\"", "-\"-a b\""),
            ("- -\"a b\"", "- \"-a b\""),
            ("k:\"v\"x", "\"k:v\"x"),
            ("k: \"v\" x", "k: \"v\" x"),
            ("k : \"v\" x", "k : \"v\" x"),
            ("k_1:\"a b\" c", "\"k_1:a b\" c"),
            ("9:\"x\" y", "\"9:x\" y"),
            ("-9:\"x y\" z", "\"-9:x y\" z"),
            ("a:b \"c:d\" e", "a:b \"c:d\" e"),
            ("title:\"a\" b:\"c\"", "\"title:a\" \"b:c\""),
            ("@\"last Tuesday\" x", "\"@last Tuesday\" x"),
            ("\"@date\" x", "\"@date\" x"),
            ("-@date:\"last Monday\"", "\"-@date:last Monday\""),
            ("x @\"a b\" y \"c d\" z", "x \"@a b\" y \"c d\" z"),
            ("\"title:a b\" c", "\"title:a b\" c"),
            ("\"-a b\" c", "\"-a b\" c"),
            ("a:-\"b c\"", "a:\"-b c\""),
            (":-\"a b\"", ":\"-a b\""),
            ("a:\"\" b:\"\"", "a:\"\" b:\"\""),
            ("k:\"v\" \"w x\" y", "\"k:v\" \"w x\" y"),
            ("k:v \"w x\" y", "k:v \"w x\" y"),
            ("k: v\"w x\" y", "k: v\"w x\" y"),
        ];
        for (input, want) in cases {
            assert_eq!(preprocess(input), *want, "input: {input:?}");
        }
    }

    // ---- PDO::quote (pgsql driver, oracle-verified) ----------------------
    #[test]
    fn pdo_quote_matches_pg_driver() {
        assert_eq!(pdo_quote(""), "''");
        assert_eq!(pdo_quote("it's"), "'it''s'");
        assert_eq!(pdo_quote("a'b'c"), "'a''b''c'");
        assert_eq!(pdo_quote(r"\n\t"), "'\\n\\t'");
        assert_eq!(pdo_quote(r"a\b"), "'a\\b'");
        assert_eq!(pdo_quote(r"\a"), "'\\a'");
        assert_eq!(pdo_quote("a\0b"), "'a'");
        assert_eq!(pdo_quote("a b c"), "'a b c'");
        assert_eq!(pdo_quote(r#"d'q"o"#), "'d''q\"o'");
    }

    #[test]
    fn php_htmlspecialchars_basic() {
        assert_eq!(
            php_htmlspecialchars("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&#039;f"
        );
        assert_eq!(php_htmlspecialchars(""), "");
    }
}
