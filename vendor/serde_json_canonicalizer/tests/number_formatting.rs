use std::io;

use serde::Serialize;
use test_case::case;

use serde_json_canonicalizer::to_vec;

fn value_to_string<V: Serialize>(value: V) -> io::Result<String> {
    let buffer = to_vec(&value)?;
    String::from_utf8(buffer).map_err(|_| unreachable!())
}

fn value_to_string_no_err<V: Serialize>(value: V) -> String {
    value_to_string(value).unwrap()
}

// This test is running on a generated file, the generator is copied from the reference implementation
// https://github.com/cyberphone/json-canonicalization/blob/dc406ceaf94b5fa554fcabb92c091089c2357e83/testdata/numgen.js
// To run the test, generate the input file (~3.7Gb) by executing the generation script
// in `resources/generated-numbers` by the command `node numgen.js` (requires nodejs to be installed)
// This is a long test and should be executed via
// `cargo test --tests generated_numbers -- --nocapture --include-ignored`
#[ignore]
#[test]
fn generated_numbers() {
    use std::io::{stdout, BufRead, BufReader, Write};
    let file =
        std::fs::File::open("tests/resources/generated-numbers/es6testfile100m.txt").unwrap();
    let reader = BufReader::new(file);

    let one_percent = 1000000usize;
    let mut threshold = one_percent - 1;
    let mut percent = 0;
    let mut stdout = stdout().lock();
    let mut buffer: Vec<u8> = Vec::with_capacity(32);
    for (idx, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        let parts = line.split(',').collect::<Vec<_>>();
        assert_eq!(2, parts.len());
        let input = f64::from_bits(u64::from_str_radix(parts[0], 16).unwrap());
        let output = to_vec(&input).unwrap();
        assert_eq!(
            parts[1].as_bytes(),
            &output,
            "Testing input {} parsed into {}",
            parts[0],
            input
        );
        buffer.clear();
        if idx == threshold {
            threshold += one_percent;
            percent += 1;
            if percent % 10 == 0 {
                writeln!(stdout, "{percent}%").ok();
            } else {
                write!(stdout, ".").ok();
            }
            stdout.flush().ok();
        }
    }
    writeln!(stdout).ok();
}

#[case(0x0000000000000000 => "0" ; "Zero")]
#[case(0x8000000000000000 => "0" ; "Minus zero")]
#[case(0x0000000000000001 => "5e-324" ; "Min pos number")]
#[case(0x8000000000000001 => "-5e-324" ; "Min neg number")]
#[case(0x7fefffffffffffff => "1.7976931348623157e+308" ; "Max pos number")]
#[case(0xffefffffffffffff => "-1.7976931348623157e+308" ; "Max neg number")]
#[case(0x4340000000000000 => "9007199254740992" ; "Max pos int")]
#[case(0xc340000000000000 => "-9007199254740992" ; "Max neg int")]
#[case(0x4430000000000000 => "295147905179352830000" ; "~2**68")]
#[case(0x44b52d02c7e14af5 => "9.999999999999997e+22" ; "example 1")]
#[case(0x44b52d02c7e14af6 => "1e+23" ; "example 2")]
#[case(0x44b52d02c7e14af7 => "1.0000000000000001e+23" ; "example 3")]
#[case(0x444b1ae4d6e2ef4e => "999999999999999700000" ; "example 4")]
#[case(0x444b1ae4d6e2ef4f => "999999999999999900000" ; "example 5")]
#[case(0x444b1ae4d6e2ef50 => "1e+21" ; "example 6")]
#[case(0x3eb0c6f7a0b5ed8c => "9.999999999999997e-7" ; "example 7")]
#[case(0x3eb0c6f7a0b5ed8d => "0.000001" ; "example 8")]
#[case(0x41b3de4355555553 => "333333333.3333332" ; "example 9")]
#[case(0x41b3de4355555554 => "333333333.33333325" ; "example 10")]
#[case(0x41b3de4355555555 => "333333333.3333333" ; "example 11")]
#[case(0x41b3de4355555556 => "333333333.3333334" ; "example 12")]
#[case(0x41b3de4355555557 => "333333333.33333343" ; "example 13")]
#[case(0xbecbf647612f3696 => "-0.0000033333333333333333" ; "example 14")]
#[case(0x43143ff3c1cb0959 => "1424953923781206.2" ; "round to even")]
fn jcs_rfc_appendix_b(bits: u64) -> String {
    let number: f64 = f64::from_bits(bits);
    value_to_string_no_err(number)
}

#[case(f64::NAN ; "NaN")]
#[case(f64::INFINITY ; "Infinity")]
#[case(f64::NEG_INFINITY ; "-Infinity")]
fn infinite_numbers(number: f64) {
    assert!(to_vec(&number).is_err());
    assert!(value_to_string(number).is_err());
}

#[case(u8::MAX as f64 => "255" ; "u8::MAX exact")]
#[case(i8::MAX as f64 => "127" ; "i8::MAX exact")]
#[case(i8::MIN as f64 => "-128" ; "i8::MIN exact")]
#[case(u16::MAX as f64 => "65535" ; "u16::MAX exact")]
#[case(i16::MAX as f64 => "32767" ; "i16::MAX exact")]
#[case(i16::MIN as f64 => "-32768" ; "i16::MIN exact")]
#[case(u32::MAX as f64 => "4294967295" ; "u32::MAX exact")]
#[case(i32::MAX as f64 => "2147483647" ; "i32::MAX exact")]
#[case(i32::MIN as f64 => "-2147483648" ; "i32::MIN exact")]
// The RFC states that "For maximum compliance with the ECMAScript "JSON"
// object, values that are to be interpreted as true integers SHOULD be in
// the range -9007199254740991 to 9007199254740991. However, how numbers are
// used in applications does not affect the JCS algorithm."
// We check that our implementation matches with what v8 JSON.stringify does
// as the most widely used javascript engine.
#[case(u64::MAX as f64 => "18446744073709552000" ; "u64::MAX exact (v8 compat)")]
#[case(i64::MAX as f64 => "9223372036854776000" ; "i64::MAX exact (v8 compat)")]
#[case(i64::MIN as f64 => "-9223372036854776000" ; "i64::MIN exact (v8 compat)")]
#[case(184467440737095520000u128 as f64 => "184467440737095500000" ; "v8 compat rounding 1")]
#[case(1844674407370955200000u128 as f64 => "1.8446744073709552e+21" ; "v8 compat rounding 2")]
#[case(184467440737095520000000u128 as f64 => "1.844674407370955e+23" ; "v8 compat rounding 3")]
fn integers(number: f64) -> String {
    value_to_string_no_err(number)
}

#[case(u8::MAX => "255" ; "u8::MAX exact")]
fn integers_u8(number: u8) -> String {
    value_to_string_no_err(number)
}

#[case(i8::MAX => "127" ; "i8::MAX exact")]
#[case(i8::MIN => "-128" ; "i8::MIN exact")]
fn integers_i8(number: i8) -> String {
    value_to_string_no_err(number)
}

#[case(u16::MAX => "65535" ; "u16::MAX exact")]
fn integers_u16(number: u16) -> String {
    value_to_string_no_err(number)
}

#[case(i16::MAX => "32767" ; "i16::MAX exact")]
#[case(i16::MIN => "-32768" ; "i16::MIN exact")]
fn integers_i16(number: i16) -> String {
    value_to_string_no_err(number)
}

#[case(u32::MAX => "4294967295" ; "u32::MAX exact")]
fn integers_u32(number: u32) -> String {
    value_to_string_no_err(number)
}

#[case(i32::MAX => "2147483647" ; "i32::MAX exact")]
#[case(i32::MIN => "-2147483648" ; "i32::MIN exact")]
fn integers_i32(number: i32) -> String {
    value_to_string_no_err(number)
}

// The RFC states that "For maximum compliance with the ECMAScript "JSON"
// object, values that are to be interpreted as true integers SHOULD be in
// the range -9007199254740991 to 9007199254740991. However, how numbers are
// used in applications does not affect the JCS algorithm."
// We check that our implementation matches with what v8 JSON.stringify does
// as the most widely used javascript engine.
#[case(u64::MAX => "18446744073709552000" ; "u64::MAX exact (v8 compat)")]
fn integers_u64(number: u64) -> String {
    value_to_string_no_err(number)
}

#[case(i64::MAX => "9223372036854776000" ; "i64::MAX exact (v8 compat)")]
#[case(i64::MIN => "-9223372036854776000" ; "i64::MIN exact (v8 compat)")]
fn integers_i64(number: i64) -> String {
    value_to_string_no_err(number)
}

#[case(184467440737095520000u128 => "184467440737095500000" ; "v8 compat 1")]
#[case(1844674407370955200000u128 => "1.8446744073709552e+21" ; "v8 compat 2")]
#[case(184467440737095520000000u128 => "1.844674407370955e+23" ; "v8 compat 3")]
fn integers_u128(number: u128) -> String {
    value_to_string_no_err(number)
}

#[case(-184467440737095520000i128 => "-184467440737095500000" ; "v8 compat 1")]
#[case(-1844674407370955200000i128 => "-1.8446744073709552e+21" ; "v8 compat 2")]
#[case(-184467440737095520000000i128 => "-1.844674407370955e+23" ; "v8 compat 3")]
fn integers_i128(number: i128) -> String {
    value_to_string_no_err(number)
}

#[case(0.0f64 => "0" ; "zero")]
#[case(-0.0f64 => "0" ; "minus zero")]
fn zeroes(number: f64) -> String {
    value_to_string_no_err(number)
}

// https://github.com/evik42/serde-json-canonicalizer/issues/5
#[test]
fn test_issue_5() {
    let simple_input: &str = r#"{
        "number": 300
    }"#;

    let f: serde_json::Value = serde_json::from_str(simple_input).unwrap();

    let json_string = value_to_string(&f).unwrap();

    assert_eq!(json_string, r#"{"number":300}"#);
}
