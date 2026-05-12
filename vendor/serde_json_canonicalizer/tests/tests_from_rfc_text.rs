use serde_json::Value;
use test_case::test_case;

use serde_json_canonicalizer::*;

fn read_files(filename: &str) -> (Value, Vec<u8>) {
    let (input_file, expected_file) = {
        let mut resources_folder = std::path::PathBuf::from("tests/resources");
        resources_folder.push(filename);
        let mut input_file = resources_folder.clone();
        input_file.set_extension("input.json");
        let mut expected_file = resources_folder.clone();
        expected_file.set_extension("expected.json");
        (input_file, expected_file)
    };
    let input_bytes = std::fs::read(input_file).unwrap();
    let input = serde_json::from_slice(&input_bytes).unwrap();
    let expected = std::fs::read_to_string(expected_file).unwrap();
    (input, expected.into_bytes())
}

#[test]
fn example_to_utf8_vec() {
    let expected = "\
        7b 22 6c 69 74 65 72 61 6c 73 22 3a 5b 6e 75 6c 6c 2c 74 72 \
        75 65 2c 66 61 6c 73 65 5d 2c 22 6e 75 6d 62 65 72 73 22 3a \
        5b 33 33 33 33 33 33 33 33 33 2e 33 33 33 33 33 33 33 2c 31 \
        65 2b 33 30 2c 34 2e 35 2c 30 2e 30 30 32 2c 31 65 2d 32 37 \
        5d 2c 22 73 74 72 69 6e 67 22 3a 22 e2 82 ac 24 5c 75 30 30 \
        30 66 5c 6e 41 27 42 5c 22 5c 5c 5c 5c 5c 22 2f 22 7d"
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let (input, _) = read_files("rfc_example");
    assert_eq!(expected, to_vec(&input).unwrap());
}

#[test_case("rfc_example" ; "rfc example in section 3.2.2")]
#[test_case("rfc_sorting" ; "rfc sorting example in section 3.2.3")]
fn files(filename: &str) {
    let (input, expected) = read_files(filename);
    assert_eq!(expected, to_vec(&input).unwrap());
}
