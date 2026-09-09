//! Differential tests: the Rust param-parsing helpers must behave *exactly*
//! like the PHP `Handler` methods, because the front-end is written against
//! the PHP backend's input contract.
//!
//! The expected outputs in the fixtures were recorded once from the frozen
//! PHP reference (oracle app container, commit 2f7d831) — see the recorder
//! scripts under `oracle/`. Both the inputs (front-end contract) and the
//! outputs (frozen PHP) are stable, so these fixtures never need re-recording.
//!
//! Comparison semantics: `Handler::_param_to_int_array` returns a PHP array
//! that can carry *sparse* integer keys (e.g. `"1,,2"` → `[0=>1, 2=>2]`),
//! which `json_encode` renders as an object. We compare the **ordered list of
//! values** (the logical id list), not the raw keying, so the recorder
//! normalizes with `array_values` before encoding.

use serde_json::{Map, Value};
use ttrss_server::util::param_int_array;

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
