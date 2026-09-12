//! Differential tests: the Rust param-parsing helpers must behave *exactly*
//! like the PHP `Handler` methods, because the front-end is written against
//! the PHP backend's input contract.
//!
//! The expected outputs in the fixtures were recorded from the frozen PHP
//! reference (commit 2f7d831) by the recorder scripts under `oracle/recorders/`,
//! which drive the *real* PHP helpers in the public `php:8.5-cli` image (matching
//! the app's PHP 8.5) with the frozen `tt-rss/` mounted read-only — no oracle
//! stack required. Re-record with `make record-params`. Both the inputs (front-end
//! contract) and the outputs (frozen PHP) are stable, so the fixtures rarely need
//! re-recording.
//!
//! Covered helpers (each a one-line fixture battery + `*_matches_php` test):
//!   * `param_int_array`      ~ `Handler::_param_to_int_array`
//!   * `param_to_bool`        ~ `Handler::_param_to_bool`
//!   * `clean_str`            ~ `clean()`  (exercises `strip_tags` + `php_trim`)
//!   * `php_int_cast`         ~ PHP `(int)` cast
//!
//! Comparison semantics: `Handler::_param_to_int_array` returns a PHP array that
//! can carry *sparse* integer keys (e.g. `"1,,2"` → `[0=>1, 2=>2]`), which
//! `json_encode` renders as an object. We compare the **ordered list of values**
//! (the logical id list), not the raw keying, so that recorder normalizes with
//! `array_values` before encoding. The other helpers compare directly (bool, the
//! cleaned string, and the int respectively).

use serde_json::{Map, Value};
use ttrss::util::{clean_str, param_int_array, param_to_bool, php_int_cast};

fn run_param_int_array(input: &Value) -> Vec<i32> {
    // The handler looks for `ids[]` then `ids`; place the raw input under
    // `ids` so the comma-string and array shapes both flow through.
    let mut extra = Map::new();
    extra.insert("ids".to_string(), input.clone());
    param_int_array(&extra, "ids")
}

#[test]
fn param_int_array_matches_php() {
    const FIXTURE: &str = include_str!("fixtures/param_int_array.json");
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(!cases.is_empty(), "fixture should not be empty");
    for (i, case) in cases.iter().enumerate() {
        let input = &case["in"];
        let expected: Vec<i32> = case["out"]
            .as_array()
            .unwrap_or_else(|| panic!("case {i}: missing 'out' array"))
            .iter()
            .map(|v| v.as_i64().expect("int") as i32)
            .collect();
        let got = run_param_int_array(input);
        assert_eq!(
            got, expected,
            "case {i}: input {input} → got {got:?}, PHP expected {expected:?}",
        );
    }
}

/// `Handler::_param_to_bool` — `clean($p) && ($p !== "f" && $p !== "false")`.
/// The front-end contract is strings + absent; bool/int inputs are included
/// to pin the Rust's extra JSON-body branches. JSON `null` maps to Rust `None`
/// (the absent case), which is also what the PHP `?? false` path yields (false).
fn run_param_to_bool(input: &Value) -> bool {
    if input.is_null() {
        param_to_bool(None)
    } else {
        param_to_bool(Some(input))
    }
}

#[test]
fn param_to_bool_matches_php() {
    const FIXTURE: &str = include_str!("fixtures/param_to_bool.json");
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(!cases.is_empty(), "fixture should not be empty");
    for (i, case) in cases.iter().enumerate() {
        let input = &case["in"];
        let expected = case["out"]
            .as_bool()
            .unwrap_or_else(|| panic!("case {i}: 'out' not a bool"));
        let got = run_param_to_bool(input);
        assert_eq!(got, expected, "case {i}: input {input} → got {got}, PHP expected {expected}");
    }
}

/// `clean()` — `trim(strip_tags($v))` for strings. Exercises the Rust's
/// hand-rolled `strip_tags`/`php_trim` against PHP's, including malformed tags.
#[test]
fn clean_str_matches_php() {
    const FIXTURE: &str = include_str!("fixtures/clean_str.json");
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(!cases.is_empty(), "fixture should not be empty");
    for (i, case) in cases.iter().enumerate() {
        let input = case["in"].as_str().expect("string input");
        let expected = case["out"].as_str().expect("string out");
        let got = clean_str(input);
        assert_eq!(got, expected, "case {i}: input {input:?} → got {got:?}, PHP expected {expected:?}");
    }
}

/// PHP's `(int)` cast — leading-integer/float prefix, whitespace-trimmed,
/// truncated, clamped to the int range on overflow. Exercises the Rust
/// `php_int_cast` approximation (including scientific notation + overflow).
#[test]
fn php_int_cast_matches_php() {
    const FIXTURE: &str = include_str!("fixtures/php_int_cast.json");
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(!cases.is_empty(), "fixture should not be empty");
    for (i, case) in cases.iter().enumerate() {
        let input = case["in"].as_str().expect("string input");
        let expected = case["out"].as_i64().expect("int out");
        let got = php_int_cast(input);
        assert_eq!(got, expected, "case {i}: input {input:?} → got {got}, PHP expected {expected}");
    }
}
