use serde_json::{json, Value};
use test_case::test_case;

use serde_json_canonicalizer::to_vec;

// These do not play well with test_case macro
#[test]
fn literals() {
    assert_eq!(
        "null",
        String::from_utf8(to_vec(&json!(null)).unwrap()).unwrap()
    );
    assert_eq!(
        "true",
        String::from_utf8(to_vec(&json!(true)).unwrap()).unwrap()
    );
    assert_eq!(
        "false",
        String::from_utf8(to_vec(&json!(false)).unwrap()).unwrap()
    );
}

#[test_case(json!(42) => "42" ; "number")]
#[test_case(json!("42") => "\"42\"" ; "string")]
#[test_case(json!([]) => "[]" ; "empty array")]
#[test_case(json!({}) => "{}" ; "empty object")]
fn basic_types(value: Value) -> String {
    String::from_utf8(to_vec(&value).unwrap()).unwrap()
}
