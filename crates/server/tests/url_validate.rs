//! Differential test: the Rust `urlhelper::validate` must behave *exactly* like
//! the PHP `UrlHelper::validate`, because feed subscribe/opensite (and the
//! feed-update fetch path) rely on the same URL normalization + validation
//! semantics.
//!
//! The expected outputs in the fixture were recorded from the frozen PHP
//! reference (commit 2f7d831) by the recorder at `oracle/recorders/url_validate.php`,
//! run in the real `ghcr.io/tt-rss/tt-rss` image (which has `intl` for
//! `idn_to_ascii`) with the frozen `tt-rss/` mounted read-only. Both the inputs
//! and the frozen-PHP outputs are stable, so the fixture rarely needs
//! re-recording.
//!
//! Comparison: for each `{in, ext, out}` case, `validate(in, ext)` must equal
//! `out` (a string) or be `None` when PHP returned `false`.

use serde_json::Value;
use ttrss_server::urlhelper::validate;

#[test]
fn url_validate_matches_php() {
    const FIXTURE: &str = include_str!("fixtures/url_validate.json");
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(!cases.is_empty(), "fixture should not be empty");

    let mut valid_count = 0usize;
    let mut invalid_count = 0usize;
    for (i, case) in cases.iter().enumerate() {
        let input = case["in"].as_str().expect("case: 'in' is a string");
        let extended = case["ext"].as_bool().expect("case: 'ext' is a bool");
        let expected = match &case["out"] {
            Value::String(s) => Some(s.clone()),
            Value::Bool(false) => None,
            other => panic!("case {i}: unexpected 'out' {other:?}"),
        };
        let got = validate(input, extended);
        match &expected {
            Some(_) => valid_count += 1,
            None => invalid_count += 1,
        }
        assert_eq!(
            got, expected,
            "case {i}: input {input:?} (ext={extended})"
        );
    }
    // sanity: the battery exercises both outcomes
    assert!(valid_count > 0 && invalid_count > 0);
}
