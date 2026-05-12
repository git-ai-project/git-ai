[![Crates.io](https://img.shields.io/crates/v/serde_json_canonicalizer.svg)](https://crates.io/crates/serde_json_canonicalizer)
[![Workflow Status](https://github.com/evik42/serde-json-canonicalizer/actions/workflows/nightly.yml/badge.svg)](https://github.com/evik42/serde-json-canonicalizer/actions/workflows/nightly.yml)
![Maintenance](https://img.shields.io/badge/maintenance-activly--developed-brightgreen.svg)

# serde_json_canonicalizer

An RFC 8785 compatible JSON Canonicalization Scheme output for [serde_json](https://crates.io/crates/serde_json).

JSON Canonicalization Scheme [RFC-8785](https://datatracker.ietf.org/doc/html/rfc8785) defines a JSON serialization scheme to allow using JSON data in cryptographic operations that rely on byte level reproduction of data. It can be used instead of storing the serialized format as a BASE64 encoded string or similar packaging, allowing easier handling of JSON formatted data that can be canonicalized before feeding it to a cryptographic function.

## Usage
```rust
use serde_json_canonicalizer::{to_string, to_vec};

#[derive(serde::Serialize)]
struct Data {
    c: isize,
    b: bool,
    a: String,
}

let data = Data { c: 120, b: false, a: "Hello!".to_string() };
let expected = r#"{"a":"Hello!","b":false,"c":120}"#;

// serlialize to string or bytes, drop-in replacement for serde_json
let json_string = to_string(&data).unwrap();
let json_bytes = to_vec(&data).unwrap();

assert_eq!(json_string, expected);
assert_eq!(json_bytes, expected.as_bytes());
```

## serde_json arbitrary precision feature

`serde_json` supports writing arbitrary precision numbers into JSON which is not conforming to the RFC 8785. Those numbers can be reliably read back only by a deserializer that is prepared to read arbitrary precision numbers that are not available in JavaScript. The canonicalization method will convert these numbers into doubles and serialize them according to the rules of representing doubles. Thus the arbitrary precision is lost.
To use numbers that are not represented as doubles, store them as strings in the JSON and rely on the consuming application to deserialize these strings accordingly. (For example storing hash values, signatures or other numbers that cannot be represented in double format.)

## License

Licensed under MIT license ([LICENSE](LICENSE) or https://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, shall be licensed as above, without any additional terms or conditions.

## Compare to [serde_jcs](https://github.com/l1h3r/serde_jcs)

I created this crate because `serde_jcs` seems to be abandoned and the issues in that repository list a few things where it differs from the RFC. This crate aims to be 100% compatible with the RFC to be a suitable implementation in a multi-language environment.
