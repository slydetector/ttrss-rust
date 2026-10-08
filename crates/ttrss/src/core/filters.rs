//! `RSSUtils::eval_article_filters` — filter-rule evaluation
//! (`tt-rss/classes/RSSUtils.php`), the pure core of the article-filter
//! engine. Runs a candidate article (title / content / link / author / tags)
//! through a list of filters and returns the matched actions plus (optionally
//! collected) the matching rules/filters.
//!
//! Used by `Pref_Filters::testFilterDo` (the "Test Filter" dialog) and, from
//! M6, by the update engine when new articles are ingested.

use regex::Regex;

/// Filter-rule field type — the `name` of `Pref_Filters::$filter_types`
/// (`type` in the PHP rule arrays).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleType {
    Title,
    Content,
    Both,
    Link,
    Author,
    Tag,
    /// `filter_type` 5 ("date" — intentionally unsupported, kept in the PHP
    /// for backward compatibility) and unknown ids: the `switch` has no case
    /// for these, so the rule never matches.
    Unknown,
}

impl RuleType {
    /// `Pref_Filters::$filter_types[<id>]['name']` — type 5 is deliberately
    /// absent ("preserving the original behavior of this type not being
    /// supported").
    pub fn from_id(id: i32) -> Self {
        match id {
            1 => RuleType::Title,
            2 => RuleType::Content,
            3 => RuleType::Both,
            4 => RuleType::Link,
            6 => RuleType::Author,
            7 => RuleType::Tag,
            _ => RuleType::Unknown,
        }
    }
}

/// A filter rule (`$filter['rules'][n]`).
#[derive(Debug, Clone)]
pub struct EvalRule {
    /// The raw pattern (no delimiters; the engine wraps it in `…/iu`).
    pub reg_exp: String,
    pub rule_type: RuleType,
    /// Invert this rule's match result.
    pub inverse: bool,
}

/// A filter to evaluate (`$filters[n]`).
#[derive(Debug, Clone, Default)]
pub struct EvalFilter {
    /// Match if *any* rule matches (otherwise all must match).
    pub match_any_rule: bool,
    /// Invert the whole filter's match result.
    pub inverse: bool,
    pub rules: Vec<EvalRule>,
    pub actions: Vec<FilterAction>,
}

/// A filter action (`$filter['actions'][n]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterAction {
    /// The action's `type` key (e.g. `"stop"`, `"score"`, `"label"`);
    /// `None` for non-associative actions (e.g. `testFilterDo`'s
    /// `'dummy-action'`), where PHP's `isset($action['type'])` is false.
    pub action_type: Option<String>,
    /// The action's `param` key.
    pub param: Option<String>,
}

/// The last processed rule of a matched filter, annotated with
/// `regexp_matches` (PHP adds it to the rule array).
#[derive(Debug, Clone, Default)]
pub struct MatchedRule {
    /// Index into the filter's `rules` (the last rule processed, including
    /// skipped/empty ones); `None` when the filter had no rules at all
    /// (PHP's `false`).
    pub rule_index: Option<usize>,
    /// `preg_match` captures; `[0]` is the whole match.
    pub regexp_matches: Vec<String>,
}

/// `eval_article_filters` result: the matched actions (`$matches`) plus the
/// PHP `$matched_rules` / `$matched_filters` out-parameters.
#[derive(Debug, Clone, Default)]
pub struct EvalOutcome {
    /// Actions from the matched filters, in order; evaluation stops
    /// (early-return) at a `stop` action.
    pub actions: Vec<FilterAction>,
    /// One entry per matched filter (the last processed rule + captures).
    pub matched_rules: Vec<MatchedRule>,
    /// Indices into the `filters` slice, one per matched filter.
    pub matched_filters: Vec<usize>,
}

/// PHP `empty()` for the string cases (`""` and `"0"`).
fn php_empty(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// `@preg_match("/$pattern/iu", $subject, $matches)` — `None` on an invalid
/// pattern (the `@` suppresses the warning; no match), otherwise the capture
/// list (`[0]` = whole match, then participating groups, mirroring PHP's
/// `$matches`). The `i`/`u` PCRE modifiers (case-insensitive, UTF mode)
/// correspond to the `regex` crate's Unicode-by-default, `(?i)` matching.
fn preg_match_ci(pattern: &str, subject: &str) -> Option<Vec<String>> {
    let Ok(re) = Regex::new(&format!("(?i){pattern}")) else {
        return None;
    };
    let caps = re.captures(subject)?;
    Some(
        (0..caps.len())
            .filter_map(|i| caps.get(i).map(|m| m.as_str().to_string()))
            .collect(),
    )
}

/// `RSSUtils::eval_article_filters($filters, $title, $content, $link,
/// $author, $tags, &$matched_rules, &$matched_filters)`.
///
/// The returned [`EvalOutcome`] holds `$matches` (`.actions`) and both
/// out-parameters (`.matched_rules`, `.matched_filters`).
pub fn eval_article_filters(
    filters: &[EvalFilter],
    title: &str,
    content: &str,
    link: &str,
    author: &str,
    tags: &[String],
) -> EvalOutcome {
    let mut out = EvalOutcome::default();

    // PHP strips `[\r\n\t]` from `$content` on first use by a
    // `content`/`both` rule; the result is idempotent, so do it once up
    // front.
    let content_flat: String = content
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\t'))
        .collect();

    for (fi, filter) in filters.iter().enumerate() {
        let mut filter_match = false;
        let mut last_rule: Option<usize> = None;
        let mut regexp_matches: Vec<String> = Vec::new();

        for (ri, rule) in filter.rules.iter().enumerate() {
            let mut rule_match = false;
            // `$last_processed_rule = $rule;` runs even for skipped rules.
            last_rule = Some(ri);

            // `str_replace('/', '\/', …)` then strip `\n` (textarea CR/LF).
            let reg_exp = rule.reg_exp.replace('/', "\\/").replace('\n', "");
            if php_empty(&reg_exp) {
                continue; // `empty($reg_exp)` → `continue`
            }

            match rule.rule_type {
                RuleType::Title => {
                    rule_match = match preg_match_ci(&reg_exp, title) {
                        Some(caps) => {
                            regexp_matches = caps;
                            true
                        }
                        None => {
                            regexp_matches.clear();
                            false
                        }
                    };
                }
                RuleType::Content => {
                    rule_match = match preg_match_ci(&reg_exp, &content_flat) {
                        Some(caps) => {
                            regexp_matches = caps;
                            true
                        }
                        None => {
                            regexp_matches.clear();
                            false
                        }
                    };
                }
                RuleType::Both => {
                    // `(preg_match(title) || preg_match(content))` — the
                    // title attempt happens first; its captures win when it
                    // matches (the content attempt is short-circuited).
                    match preg_match_ci(&reg_exp, title) {
                        Some(caps) => {
                            regexp_matches = caps;
                            rule_match = true;
                        }
                        None => match preg_match_ci(&reg_exp, &content_flat) {
                            Some(caps) => {
                                regexp_matches = caps;
                                rule_match = true;
                            }
                            None => {
                                regexp_matches.clear();
                            }
                        },
                    }
                }
                RuleType::Link => {
                    rule_match = match preg_match_ci(&reg_exp, link) {
                        Some(caps) => {
                            regexp_matches = caps;
                            true
                        }
                        None => {
                            regexp_matches.clear();
                            false
                        }
                    };
                }
                RuleType::Author => {
                    rule_match = match preg_match_ci(&reg_exp, author) {
                        Some(caps) => {
                            regexp_matches = caps;
                            true
                        }
                        None => {
                            regexp_matches.clear();
                            false
                        }
                    };
                }
                RuleType::Tag => {
                    // `if (count($tags) == 0) $tags[] = '';` — allow matching
                    // (against the empty string) when the article has no tags.
                    if tags.is_empty() {
                        rule_match = match preg_match_ci(&reg_exp, "") {
                            Some(caps) => {
                                regexp_matches = caps;
                                true
                            }
                            None => {
                                regexp_matches.clear();
                                false
                            }
                        };
                    } else {
                        for tag in tags {
                            rule_match = match preg_match_ci(&reg_exp, tag) {
                                Some(caps) => {
                                    regexp_matches = caps;
                                    true
                                }
                                None => {
                                    regexp_matches.clear();
                                    false
                                }
                            };
                            if rule_match {
                                break;
                            }
                        }
                    }
                }
                RuleType::Unknown => {} // no `switch` case → no match
            }

            if rule.inverse {
                rule_match = !rule_match;
            }

            if filter.match_any_rule {
                if rule_match {
                    filter_match = true;
                    break;
                }
            } else {
                filter_match = rule_match;
                if !rule_match {
                    break;
                }
            }
        }

        if filter.inverse {
            filter_match = !filter_match;
        }

        if filter_match {
            out.matched_rules.push(MatchedRule {
                rule_index: last_rule,
                regexp_matches,
            });
            out.matched_filters.push(fi);
            for action in &filter.actions {
                out.actions.push(action.clone());
                // `isset($action['type']) && $action['type'] == 'stop'`
                if action.action_type.as_deref() == Some("stop") {
                    return out;
                }
            }
        }
    }

    out
}

/// PHP `RSSUtils::has_article_filter_action($filter_actions, $type)` — true if
/// any matched action has the given type.
pub fn has_article_filter_action(actions: &[FilterAction], action_type: &str) -> bool {
    actions
        .iter()
        .any(|a| a.action_type.as_deref() == Some(action_type))
}

/// PHP `RSSUtils::find_article_filter_actions($filter_actions, $type)` — the
/// actions whose type exactly equals `$type` (re-indexed from zero).
pub fn find_article_filter_actions(
    actions: &[FilterAction],
    action_type: &str,
) -> Vec<FilterAction> {
    actions
        .iter()
        .filter(|a| a.action_type.as_deref() == Some(action_type))
        .cloned()
        .collect()
}

/// PHP `RSSUtils::calculate_article_score($filter_actions)` — the sum of the
/// `param` (as an int) of every `score` action.
pub fn calculate_article_score(actions: &[FilterAction]) -> i64 {
    actions
        .iter()
        .filter(|a| a.action_type.as_deref() == Some("score"))
        .map(|a| a.param.as_deref().unwrap_or("").parse::<i64>().unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    //! 1:1 ports of `tt-rss/tests_functional/RSSUtilsTest.php`
    //! `test_eval_article_filters_*` (same inputs, same expectations). The
    //! PHP `$matched_filters[0]["id"]` assertion becomes an index assertion
    //! (the engine reports the input position of the matched filter).

    use super::*;

    fn rule(reg_exp: &str, rule_type: RuleType, inverse: bool) -> EvalRule {
        EvalRule {
            reg_exp: reg_exp.into(),
            rule_type,
            inverse,
        }
    }

    fn action(t: &str, param: &str) -> FilterAction {
        FilterAction {
            action_type: Some(t.into()),
            param: if param.is_empty() {
                None
            } else {
                Some(param.into())
            },
        }
    }

    fn f(
        match_any_rule: bool,
        inverse: bool,
        rules: Vec<EvalRule>,
        actions: Vec<FilterAction>,
    ) -> EvalFilter {
        EvalFilter {
            match_any_rule,
            inverse,
            rules,
            actions,
        }
    }

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn eval_article_filters_title_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, false)],
            vec![action("label", "TestLabel")],
        )];
        let out = eval_article_filters(&filters, "This is a test article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.actions[0].action_type.as_deref(), Some("label"));
    }

    #[test]
    fn eval_article_filters_title_no_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("xyzxyz", RuleType::Title, false)],
            vec![action("label", "TestLabel")],
        )];
        let out = eval_article_filters(&filters, "This is a test article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_content_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("secret", RuleType::Content, false)],
            vec![action("label", "Secret")],
        )];
        let out = eval_article_filters(
            &filters,
            "Title",
            "This contains a secret message",
            "",
            "",
            &[],
        );
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_author_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("john", RuleType::Author, false)],
            vec![action("label", "John")],
        )];
        let out = eval_article_filters(&filters, "Title", "", "", "John Doe", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_link_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("example\\.com", RuleType::Link, false)],
            vec![action("label", "Example")],
        )];
        let out = eval_article_filters(&filters, "Title", "", "https://example.com/post", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_tag_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("news", RuleType::Tag, false)],
            vec![action("label", "News")],
        )];
        let out = eval_article_filters(&filters, "Title", "", "", "", &tags(&["news", "world"]));
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_tag_no_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("sports", RuleType::Tag, false)],
            vec![action("label", "Sports")],
        )];
        let out = eval_article_filters(&filters, "Title", "", "", "", &tags(&["news", "world"]));
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_both_title_and_content() {
        let filters = vec![f(
            false,
            false,
            vec![rule("urgent", RuleType::Both, false)],
            vec![action("score", "100")],
        )];
        // Match in content
        let out = eval_article_filters(&filters, "Title", "This is urgent", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.actions[0].action_type.as_deref(), Some("score"));
    }

    #[test]
    fn eval_article_filters_inverse_rule() {
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, true)],
            vec![action("label", "NotTest")],
        )];
        // Inverse: should match when title does NOT contain "test"
        let out = eval_article_filters(&filters, "This is normal", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_inverse_rule_no_match() {
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, true)],
            vec![action("label", "NotTest")],
        )];
        // Inverse: should NOT match when title DOES contain "test"
        let out = eval_article_filters(&filters, "This is a test", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_inverse_filter() {
        let filters = vec![f(
            false,
            true,
            vec![rule("test", RuleType::Title, false)],
            vec![action("label", "NotTest")],
        )];
        // Inverse filter: should match when rules do NOT match
        let out = eval_article_filters(&filters, "Normal article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_match_any_rule() {
        let filters = vec![f(
            true,
            false,
            vec![
                rule("xyz", RuleType::Title, false),
                rule("john", RuleType::Author, false),
            ],
            vec![action("label", "MatchAny")],
        )];
        // match_any_rule: author matches, title doesn't — filter should still match
        let out = eval_article_filters(&filters, "No xyz here", "", "", "John Doe", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_match_all_rule() {
        let filters = vec![f(
            false,
            false,
            vec![
                rule("hello", RuleType::Title, false),
                rule("john", RuleType::Author, false),
            ],
            vec![action("label", "AllMatch")],
        )];
        // match_all (default): both title AND author must match
        let out = eval_article_filters(&filters, "Hello from John", "", "", "John Doe", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_match_all_rule_partial() {
        let filters = vec![f(
            false,
            false,
            vec![
                rule("hello", RuleType::Title, false),
                rule("john", RuleType::Author, false),
            ],
            vec![action("label", "AllMatch")],
        )];
        // match_all: title matches but author doesn't — filter should NOT match
        let out = eval_article_filters(&filters, "Hello World", "", "", "Jane Doe", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_stop_action() {
        let filters = vec![
            f(
                false,
                false,
                vec![rule("urgent", RuleType::Title, false)],
                vec![action("label", "Urgent"), action("stop", "")],
            ),
            f(
                false,
                false,
                vec![rule("urgent", RuleType::Title, false)],
                vec![action("label", "AlsoUrgent")],
            ),
        ];
        let out = eval_article_filters(&filters, "Urgent news", "", "", "", &[]);
        // First filter matches and has stop action, so second filter should not be evaluated
        assert_eq!(out.actions.len(), 2);
        assert_eq!(out.actions[0].action_type.as_deref(), Some("label"));
        assert_eq!(out.actions[1].action_type.as_deref(), Some("stop"));
    }

    #[test]
    fn eval_article_filters_multiple_filters() {
        let filters = vec![
            f(
                false,
                false,
                vec![rule("test", RuleType::Title, false)],
                vec![action("label", "Label1")],
            ),
            f(
                false,
                false,
                vec![rule("secret", RuleType::Content, false)],
                vec![action("label", "Label2")],
            ),
        ];
        let out = eval_article_filters(
            &filters,
            "Test article",
            "Contains secret info",
            "",
            "",
            &[],
        );
        assert_eq!(out.actions.len(), 2);
    }

    #[test]
    fn eval_article_filters_matched_rules_and_filters() {
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, false)],
            vec![action("label", "TestLabel")],
        )];
        let out = eval_article_filters(&filters, "Test article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.matched_rules.len(), 1);
        assert_eq!(out.matched_filters, vec![0]); // PHP: $matched_filters[0]["id"] == 1
        // The matching rule's captures are recorded on the matched rule.
        assert_eq!(out.matched_rules[0].rule_index, Some(0));
        assert_eq!(
            out.matched_rules[0].regexp_matches,
            vec!["Test".to_string()]
        ); // case preserved
    }

    #[test]
    fn eval_article_filters_score_action() {
        let filters = vec![f(
            false,
            false,
            vec![rule("important", RuleType::Title, false)],
            vec![action("score", "50")],
        )];
        let out = eval_article_filters(&filters, "Important news", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.actions[0].action_type.as_deref(), Some("score"));
        assert_eq!(out.actions[0].param.as_deref(), Some("50"));
    }

    #[test]
    fn eval_article_filters_complex_regexp() {
        let filters = vec![f(
            false,
            false,
            vec![rule("^[A-Z][a-z]+\\s[A-Z][a-z]+$", RuleType::Title, false)],
            vec![action("label", "ProperNames")],
        )];
        let out = eval_article_filters(&filters, "John Doe", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_filter_with_multiple_actions() {
        let filters = vec![f(
            false,
            false,
            vec![rule("critical", RuleType::Title, false)],
            vec![
                action("label", "Critical"),
                action("score", "100"),
                action("mark", ""),
            ],
        )];
        let out = eval_article_filters(&filters, "Critical update", "", "", "", &[]);
        assert_eq!(out.actions.len(), 3);
        assert_eq!(out.actions[0].action_type.as_deref(), Some("label"));
        assert_eq!(out.actions[1].action_type.as_deref(), Some("score"));
        assert_eq!(out.actions[2].action_type.as_deref(), Some("mark"));
    }

    #[test]
    fn eval_article_filters_empty_filters() {
        let out = eval_article_filters(&[], "Title", "Content", "Link", "Author", &tags(&["tag1"]));
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_empty_regexp_skipped() {
        let filters = vec![f(
            false,
            false,
            vec![rule("", RuleType::Title, false)],
            vec![action("label", "ShouldNotMatch")],
        )];
        let out = eval_article_filters(&filters, "Any title", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_tag_with_no_tags() {
        let filters = vec![f(
            false,
            false,
            vec![rule("empty", RuleType::Tag, false)],
            vec![action("label", "Empty")],
        )];
        // With no tags, the filter should not match (empty tag array gets a dummy '' tag)
        let out = eval_article_filters(&filters, "Title", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_filter_with_no_actions() {
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, false)],
            vec![],
        )];
        let out = eval_article_filters(&filters, "Test article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_filter_with_no_rules() {
        let filters = vec![f(false, false, vec![], vec![action("label", "NoRules")])];
        let out = eval_article_filters(&filters, "Test article", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    // --- engine-specific behavior the PHP suite pins down implicitly ---

    #[test]
    fn eval_article_filters_inverted_filter_without_rules_matches_all() {
        // PHP: no rules → rules pass vacuously; the filter-level `inverse`
        // then flips the result.
        let filters = vec![f(false, true, vec![], vec![action("label", "All")])];
        let out = eval_article_filters(&filters, "Anything", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.matched_rules[0].rule_index, None);
        assert!(out.matched_rules[0].regexp_matches.is_empty());
    }

    #[test]
    fn eval_article_filters_slash_and_newline_normalization() {
        // `a/b` (unescaped slash) and embedded `\n` must not break the
        // pattern the way a raw PHP pattern would; the engine escapes `/`
        // and strips newlines before compiling.
        let filters = vec![f(
            false,
            false,
            vec![rule("a/b", RuleType::Title, false)],
            vec![action("label", "X")],
        )];
        let out = eval_article_filters(&filters, "za/bq", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_invalid_pattern_is_no_match() {
        // `@preg_match` suppression: an invalid pattern is not an error, it
        // simply doesn't match.
        let filters = vec![f(
            false,
            false,
            vec![rule("(unclosed", RuleType::Title, false)],
            vec![action("label", "X")],
        )];
        let out = eval_article_filters(&filters, "(unclosed", "", "", "", &[]);
        assert_eq!(out.actions.len(), 0);
    }

    #[test]
    fn eval_article_filters_case_insensitive_match() {
        // The `i` modifier: uppercase subject matches a lowercase pattern.
        let filters = vec![f(
            false,
            false,
            vec![rule("test", RuleType::Title, false)],
            vec![action("label", "X")],
        )];
        let out = eval_article_filters(&filters, "THIS IS A TEST", "", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn eval_article_filters_both_title_match_wins_captures() {
        // `both`: when the title matches, the title's captures win (the
        // content attempt is short-circuited).
        let filters = vec![f(
            false,
            false,
            vec![rule("word", RuleType::Both, false)],
            vec![action("label", "X")],
        )];
        let out = eval_article_filters(&filters, "the Word here", "word in content", "", "", &[]);
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.matched_rules[0].regexp_matches[0], "Word");
    }

    // ------------------------------------------------------------------------
    // has_article_filter_action() — filter action type check
    // ------------------------------------------------------------------------

    #[test]
    fn has_article_filter_action_found() {
        let actions = [action("label", "Important"), action("score", "10")];
        assert!(has_article_filter_action(&actions, "label"));
    }

    #[test]
    fn has_article_filter_action_not_found() {
        let actions = [action("label", "Important"), action("score", "10")];
        assert!(!has_article_filter_action(&actions, "publish"));
    }

    #[test]
    fn has_article_filter_action_empty_array() {
        assert!(!has_article_filter_action(&[], "label"));
    }

    #[test]
    fn has_article_filter_action_type_case_sensitive() {
        let actions = [action("label", "Important")];
        assert!(!has_article_filter_action(&actions, "Label"));
    }

    #[test]
    fn has_article_filter_action_multiple_types() {
        let actions = [
            action("label", "A"),
            action("tag", "B"),
            action("catchup", ""),
        ];
        assert!(has_article_filter_action(&actions, "catchup"));
        assert!(!has_article_filter_action(&actions, "mark"));
    }

    // ------------------------------------------------------------------------
    // find_article_filter_actions() — filter action type filtering
    // ------------------------------------------------------------------------

    #[test]
    fn find_article_filter_actions_single_match() {
        let actions = [action("label", "Important"), action("score", "10")];
        let result = find_article_filter_actions(&actions, "label");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].action_type.as_deref(), Some("label"));
        assert_eq!(result[0].param.as_deref(), Some("Important"));
    }

    #[test]
    fn find_article_filter_actions_multiple_matches() {
        let actions = [
            action("score", "5"),
            action("label", "A"),
            action("score", "10"),
        ];
        let result = find_article_filter_actions(&actions, "score");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].param.as_deref(), Some("5"));
        assert_eq!(result[1].param.as_deref(), Some("10"));
    }

    #[test]
    fn find_article_filter_actions_no_match() {
        let actions = [action("label", "Important"), action("score", "10")];
        let result = find_article_filter_actions(&actions, "publish");
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn find_article_filter_actions_empty_input() {
        let result = find_article_filter_actions(&[], "label");
        assert_eq!(result.len(), 0);
    }

    // ------------------------------------------------------------------------
    // calculate_article_score() — score modifier summation
    // ------------------------------------------------------------------------

    #[test]
    fn calculate_article_score_single_modifier() {
        let actions = [action("score", "10")];
        assert_eq!(calculate_article_score(&actions), 10);
    }

    #[test]
    fn calculate_article_score_multiple_modifiers() {
        let actions = [
            action("score", "5"),
            action("label", "Important"),
            action("score", "15"),
        ];
        assert_eq!(calculate_article_score(&actions), 20);
    }

    #[test]
    fn calculate_article_score_negative_modifier() {
        let actions = [action("score", "10"), action("score", "-5")];
        assert_eq!(calculate_article_score(&actions), 5);
    }

    #[test]
    fn calculate_article_score_empty_array() {
        assert_eq!(calculate_article_score(&[]), 0);
    }

    #[test]
    fn calculate_article_score_no_score_actions() {
        let actions = [action("label", "Important"), action("tag", "news")];
        assert_eq!(calculate_article_score(&actions), 0);
    }

    #[test]
    fn calculate_article_score_zero_modifier() {
        let actions = [action("score", "0")];
        assert_eq!(calculate_article_score(&actions), 0);
    }

    #[test]
    fn calculate_article_score_large_values() {
        let actions = [action("score", "1000"), action("score", "-500")];
        assert_eq!(calculate_article_score(&actions), 500);
    }
}
