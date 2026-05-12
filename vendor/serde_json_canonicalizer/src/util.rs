//! Utility functions for JCS serialization and deserialization.
//!
//! Intended to be drop-in replacements for the [serde_json] equivalents.

use crate::jcs::JcsSerializer;
use serde::Serialize;
use std::io;

/// Serialize the given data structure as a JCS byte vector.
///
/// Drop in replacement for [serde_json::to_vec].
///
/// # Errors
///
/// Serialization can fail if `S`'s implementation of `Serialize` decides to
/// fail, or if `S` contains a map with non-string keys.
pub fn to_vec<S: Serialize>(value: &S) -> serde_json::Result<Vec<u8>> {
    // copying the serde_json::to_vec buffer size
    let mut buffer = Vec::with_capacity(128);
    to_writer(value, &mut buffer).map(|_| buffer)
}

/// Serialize the given data structure as a JCS UTF-8 string.
///
/// Drop in replacement for [serde_json::to_string].
///
/// # Errors
///
/// Serialization can fail if `S`'s implementation of `Serialize` decides to
/// fail, or if `S` contains a map with non-string keys.
pub fn to_string<S: Serialize>(value: &S) -> serde_json::Result<String> {
    to_vec(value).map(|vec| unsafe {
        // We do not emit invalid UTF-8.
        String::from_utf8_unchecked(vec)
    })
}

/// Serialize the given data structure as JCS into the I/O stream.
/// Serialization guarantees it only feeds valid UTF-8 sequences to the writer.
///
/// Drop in replacement for [serde_json::to_writer].
///
/// # Errors
///
/// Serialization can fail if `S`'s implementation of `Serialize` decides to
/// fail, or if `S` contains a map with non-string keys.
pub fn to_writer<S: Serialize, W: io::Write>(value: &S, writer: &mut W) -> serde_json::Result<()> {
    value.serialize(&mut JcsSerializer::new(writer))
}

/// Pipe a JSON formatted string into a JCS formatted string.
///
/// # Usage
/// ```
/// use serde_json_canonicalizer::pipe;
///
/// let input = r#"{"b": false, "c": 12e1, "a": "Hello!"}"#;
/// let expected = r#"{"a":"Hello!","b":false,"c":120}"#;
/// let jcs = pipe(input).unwrap();
///
/// assert_eq!(jcs, expected);
/// ```
///
/// # Errors
///
/// Deserialization uses [serde_json::from_str] directly, some failure cases include if numbers are
/// out of range, unicode lone surrogates, or other malformed JSON.
pub fn pipe(json: &str) -> serde_json::Result<String> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    to_string(&value)
}
