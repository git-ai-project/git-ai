use std::fs::read_dir;

use serde_json::Value;

use serde_json_canonicalizer::to_vec;

fn read_files(filename: &str) -> (Value, String, Vec<u8>) {
    let resources_folder = std::path::PathBuf::from("tests/resources/testdata");
    let get_path = |directory: &str, extension: &str| {
        let mut filepath = resources_folder.clone();
        filepath.push(directory);
        filepath.push(filename);
        filepath.set_extension(extension);
        filepath
    };
    let input_file = get_path("input", "json");
    let expected_string_file = get_path("output", "json");
    let expected_vec_file = get_path("outhex", "txt");

    let input_bytes = std::fs::read(input_file).unwrap();
    let input = serde_json::from_slice(&input_bytes).unwrap();
    let expected_string = std::fs::read_to_string(expected_string_file).unwrap();
    let expected_vec = std::fs::read_to_string(expected_vec_file)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (input, expected_string, expected_vec)
}

// test all files from the testdata copied from the reference implementation
#[test]
fn test_reference_testdata() {
    let test_data = read_dir("tests/resources/testdata/input").unwrap();

    for file in test_data {
        let file = file.unwrap();
        if file.file_type().unwrap().is_file()
            && file.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            let path = file.path();
            let filename = path.file_stem().unwrap().to_str().unwrap();

            let (input, expected_string, expected_vec) = read_files(filename);
            let canonicalized = to_vec(&input).unwrap();
            assert_eq!(
                expected_vec, canonicalized,
                "{} compare to binary",
                filename
            );
            assert_eq!(
                expected_string,
                String::from_utf8(canonicalized).unwrap(),
                "{} compare to string",
                filename
            );
        }
    }
}
